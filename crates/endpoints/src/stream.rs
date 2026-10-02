//! Stream payloads to agent events: each delta as `TextDelta` or `Reasoning`, with the text,
//! model, usage, and any cut-off gathered for the end. Both `chat/completions` chunks and
//! `responses` events are read, streamed or as one whole body.

use argonauts_core::AgentEvent;
use argonauts_core::settings::agents::EndpointApi;
use serde_json::Value;

/// What the whole reply came to.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Reply {
    pub text: String,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    /// What the endpoint itself charged, where it says (OpenRouter's `usage.cost`).
    pub cost_usd: Option<f64>,
    pub usage_seen: bool,
    /// The stream said the reply was done, so a closed connection isn't a cut-off one.
    pub finished: bool,
    /// Set when the endpoint said it stopped before the reply was whole.
    pub cutoff: Option<Cutoff>,
}

/// Why the endpoint ended a reply early. The text so far is still in the reply.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Cutoff {
    /// Chat's `length`, or the responses API's `max_output_tokens`.
    TokenLimit,
    ContentFilter,
    /// Another reason the endpoint named, or `None` when it gave none.
    Other(Option<String>),
}

impl Cutoff {
    fn named(reason: Option<&str>) -> Self {
        match reason {
            Some("length" | "max_tokens" | "max_output_tokens") => Self::TokenLimit,
            Some("content_filter") => Self::ContentFilter,
            other => Self::Other(other.filter(|name| !name.is_empty()).map(str::to_owned)),
        }
    }
}

/// An error the endpoint sent inside a successful response. `status` is its numeric code, a
/// guess from a named code, or 0 when there's neither.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StreamFailure {
    pub status: u16,
    pub body: String,
}

#[derive(Debug)]
pub(crate) struct ReplyStream {
    api: EndpointApi,
    reply: Reply,
    /// The model last announced, kept across attempts so a retry doesn't repeat it.
    announced: Option<String>,
}

impl ReplyStream {
    pub(crate) fn new(api: EndpointApi) -> Self {
        Self {
            api,
            reply: Reply::default(),
            announced: None,
        }
    }

    /// A fresh stream for another attempt at the same request.
    pub(crate) fn next_attempt(&self) -> Self {
        Self {
            announced: self.announced.clone(),
            ..Self::new(self.api)
        }
    }

    /// The model the reply names, once, when it isn't the one already announced.
    pub(crate) fn announce(&mut self) -> Option<String> {
        let model = self.reply.model.as_ref()?;
        if self.announced.as_ref() == Some(model) {
            return None;
        }
        self.announced = Some(model.clone());
        Some(model.clone())
    }

    pub(crate) fn finished(&self) -> bool {
        self.reply.finished
    }

    /// Events for one event's data. `[DONE]` and lifecycle payloads give none; payloads it
    /// doesn't know become `Unknown`, so nothing is dropped.
    pub(crate) fn push(&mut self, data: &str) -> Result<Vec<AgentEvent>, StreamFailure> {
        if data.trim() == "[DONE]" {
            self.reply.finished = true;
            return Ok(Vec::new());
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return Ok(vec![AgentEvent::unknown_line(data)]);
        };
        let events = match self.api {
            EndpointApi::ChatCompletions => self.chat(&value)?,
            EndpointApi::Responses => self.responses(&value)?,
        };
        match events {
            Some(events) => Ok(events),
            None => Ok(vec![AgentEvent::Unknown { raw: value }]),
        }
    }

    /// Events for a whole response body, for a server that answered without streaming.
    pub(crate) fn push_body(&mut self, body: &str) -> Result<Vec<AgentEvent>, StreamFailure> {
        let events = self.push(body.trim())?;
        self.reply.finished = true;
        Ok(events)
    }

    pub(crate) fn finish(self) -> Reply {
        self.reply
    }

    /// `None` when the payload isn't a chat completion or chunk.
    fn chat(&mut self, value: &Value) -> Result<Option<Vec<AgentEvent>>, StreamFailure> {
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            return Err(failure(error));
        }
        let choices = value.get("choices").and_then(Value::as_array);
        let usage = value.get("usage").filter(|usage| usage.is_object());
        if choices.is_none() && usage.is_none() {
            return Ok(None);
        }
        self.note_model(value);
        let mut events = Vec::new();
        // Only the first choice: requests never ask for more than one.
        if let Some(choice) = choices.and_then(|choices| choices.first()) {
            let part = choice.get("delta").or_else(|| choice.get("message"));
            if let Some(part) = part {
                // DeepSeek and vLLM call it `reasoning_content`, OpenRouter and Ollama `reasoning`.
                let reasoning = ["reasoning_content", "reasoning"]
                    .into_iter()
                    .find_map(|key| part.get(key).and_then(Value::as_str))
                    .filter(|text| !text.is_empty());
                if let Some(text) = reasoning {
                    events.push(AgentEvent::Reasoning {
                        text: text.to_owned(),
                    });
                }
                if let Some(text) = part.get("content").and_then(Value::as_str) {
                    events.extend(self.delta(text));
                }
            }
            if let Some(reason) = choice
                .get("finish_reason")
                .filter(|reason| !reason.is_null())
            {
                self.reply.finished = true;
                if let Some(name @ ("length" | "content_filter")) = reason.as_str() {
                    self.reply.cutoff = Some(Cutoff::named(Some(name)));
                }
            }
        }
        if let Some(usage) = usage {
            self.note_usage(
                usage,
                "prompt_tokens",
                "completion_tokens",
                "prompt_tokens_details",
            );
        }
        Ok(Some(events))
    }

    /// `None` when the payload isn't a `responses` event or object.
    fn responses(&mut self, value: &Value) -> Result<Option<Vec<AgentEvent>>, StreamFailure> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
        let events = match kind {
            "response.output_text.delta" => self.delta(text("delta")),
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let delta = text("delta");
                if delta.is_empty() {
                    Vec::new()
                } else {
                    vec![AgentEvent::Reasoning {
                        text: delta.to_owned(),
                    }]
                }
            }
            "response.completed" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                self.whole_response(response)
            }
            "response.incomplete" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                let events = self.whole_response(response);
                self.reply.cutoff.get_or_insert(Cutoff::Other(None));
                events
            }
            "response.failed" => {
                let error = value
                    .get("response")
                    .and_then(|response| response.get("error"))
                    .unwrap_or(value);
                return Err(failure(error));
            }
            "error" => return Err(failure(value.get("error").unwrap_or(value))),
            "response.created" | "response.in_progress" | "response.queued" => {
                if let Some(response) = value.get("response") {
                    self.note_model(response);
                }
                Vec::new()
            }
            "response.output_item.added"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.output_text.annotation.added"
            | "response.reasoning_text.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done" => Vec::new(),
            "" if value.get("object").and_then(Value::as_str) == Some("response") => {
                if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                    return Err(failure(error));
                }
                self.whole_response(value)
            }
            _ => return Ok(None),
        };
        Ok(Some(events))
    }

    /// A finished `response` object: usage, why it stopped short if it did, and its text
    /// when no delta carried it.
    fn whole_response(&mut self, response: &Value) -> Vec<AgentEvent> {
        self.note_model(response);
        self.reply.finished = true;
        let details = response
            .get("incomplete_details")
            .filter(|details| !details.is_null());
        if details.is_some() || response.get("status").and_then(Value::as_str) == Some("incomplete")
        {
            let reason = details
                .and_then(|details| details.get("reason"))
                .and_then(Value::as_str);
            self.reply.cutoff = Some(Cutoff::named(reason));
        }
        if let Some(usage) = response.get("usage").filter(|usage| usage.is_object()) {
            self.note_usage(
                usage,
                "input_tokens",
                "output_tokens",
                "input_tokens_details",
            );
        }
        if !self.reply.text.is_empty() {
            return Vec::new();
        }
        let text: String = response
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
            .filter_map(|item| item.get("content").and_then(Value::as_array))
            .flatten()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect();
        self.delta(&text)
    }

    fn delta(&mut self, text: &str) -> Vec<AgentEvent> {
        if text.is_empty() {
            return Vec::new();
        }
        self.reply.text.push_str(text);
        vec![AgentEvent::TextDelta {
            text: text.to_owned(),
            parent: None,
        }]
    }

    fn note_model(&mut self, value: &Value) {
        if self.reply.model.is_none()
            && let Some(model) = value.get("model").and_then(Value::as_str)
            && !model.is_empty()
        {
            self.reply.model = Some(model.to_owned());
        }
    }

    /// Usage totals replace earlier ones: they describe the whole reply so far.
    fn note_usage(&mut self, usage: &Value, input: &str, output: &str, details: &str) {
        let count = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or(0);
        self.reply.input_tokens = count(usage.get(input));
        self.reply.output_tokens = count(usage.get(output));
        self.reply.cached_tokens = count(usage.get(details).and_then(|d| d.get("cached_tokens")));
        if let Some(cost) = usage.get("cost").and_then(Value::as_f64) {
            self.reply.cost_usd = Some(cost);
        }
        self.reply.usage_seen = true;
    }
}

fn failure(error: &Value) -> StreamFailure {
    let status = match error.get("code") {
        Some(Value::Number(number)) => number.as_u64().and_then(|n| u16::try_from(n).ok()),
        Some(Value::String(name)) => name
            .parse()
            .ok()
            .or_else(|| name.contains("rate_limit").then_some(429)),
        _ => None,
    };
    let body = serde_json::json!({ "error": error }).to_string();
    StreamFailure {
        status: status.unwrap_or(0),
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn texts(events: &[AgentEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn chat_chunks_give_deltas_reasoning_and_usage() {
        let mut stream = ReplyStream::new(EndpointApi::ChatCompletions);
        let chunk = |delta: Value| {
            json!({"model": "m", "choices": [{"index": 0, "delta": delta, "finish_reason": null}]})
                .to_string()
        };
        let mut events = stream
            .push(&chunk(json!({"role": "assistant", "content": ""})))
            .unwrap();
        events.extend(stream.push(&chunk(json!({"reasoning": "think"}))).unwrap());
        events.extend(stream.push(&chunk(json!({"content": "Hi "}))).unwrap());
        events.extend(stream.push(&chunk(json!({"content": "there"}))).unwrap());
        let usage = json!({"choices": [], "usage": {"prompt_tokens": 9, "completion_tokens": 2,
                           "prompt_tokens_details": {"cached_tokens": 4}, "cost": 0.5}});
        events.extend(stream.push(&usage.to_string()).unwrap());
        assert!(stream.push("[DONE]").unwrap().is_empty());
        assert_eq!(texts(&events), ["Hi ", "there"]);
        assert!(matches!(&events[0], AgentEvent::Reasoning { text } if text == "think"));
        let reply = stream.finish();
        assert_eq!(reply.text, "Hi there");
        assert_eq!(reply.model.as_deref(), Some("m"));
        assert_eq!(
            (reply.input_tokens, reply.output_tokens, reply.cached_tokens),
            (9, 2, 4)
        );
        assert_eq!(reply.cost_usd, Some(0.5));
        assert!(reply.finished && reply.usage_seen);
    }

    #[test]
    fn unknown_payloads_are_kept() {
        let mut stream = ReplyStream::new(EndpointApi::ChatCompletions);
        let events = stream.push(r#"{"ping": true}"#).unwrap();
        assert_eq!(
            events,
            [AgentEvent::Unknown {
                raw: json!({"ping": true})
            }]
        );
        let events = stream.push("not json").unwrap();
        assert_eq!(
            events,
            [AgentEvent::Unknown {
                raw: json!("not json")
            }]
        );
    }

    #[test]
    fn errors_inside_a_stream_fail_it() {
        let mut stream = ReplyStream::new(EndpointApi::ChatCompletions);
        let failure = stream
            .push(r#"{"error": {"code": 502, "message": "upstream died"}}"#)
            .unwrap_err();
        assert_eq!(failure.status, 502);
        assert!(failure.body.contains("upstream died"));
        let mut stream = ReplyStream::new(EndpointApi::Responses);
        let failed = json!({"type": "response.failed",
                            "response": {"error": {"code": "rate_limit_exceeded", "message": "slow"}}});
        assert_eq!(stream.push(&failed.to_string()).unwrap_err().status, 429);
    }

    #[test]
    fn a_null_error_in_a_chat_chunk_is_not_a_failure() {
        let mut stream = ReplyStream::new(EndpointApi::ChatCompletions);
        let chunk = json!({"error": null, "choices": [{"delta": {"content": "ok"},
                           "finish_reason": "stop"}]});
        assert_eq!(texts(&stream.push(&chunk.to_string()).unwrap()), ["ok"]);
        let reply = stream.finish();
        assert!(reply.finished);
        assert_eq!(reply.cutoff, None);
    }

    #[test]
    fn chat_finish_reasons_that_cut_the_reply_short_are_kept() {
        for (reason, cutoff) in [
            ("stop", None),
            ("tool_calls", None),
            ("length", Some(Cutoff::TokenLimit)),
            ("content_filter", Some(Cutoff::ContentFilter)),
        ] {
            let mut stream = ReplyStream::new(EndpointApi::ChatCompletions);
            let text =
                json!({"choices": [{"delta": {"content": "half a rev"}, "finish_reason": null}]});
            let end = json!({"choices": [{"delta": {}, "finish_reason": reason}]});
            stream.push(&text.to_string()).unwrap();
            stream.push(&end.to_string()).unwrap();
            stream.push("[DONE]").unwrap();
            let reply = stream.finish();
            assert!(reply.finished, "{reason}");
            assert_eq!(reply.cutoff, cutoff, "{reason}");
            assert_eq!(reply.text, "half a rev");
        }
    }

    #[test]
    fn incomplete_responses_keep_their_text_and_reason() {
        let mut stream = ReplyStream::new(EndpointApi::Responses);
        let delta = json!({"type": "response.output_text.delta", "delta": "half a rev"});
        stream.push(&delta.to_string()).unwrap();
        let incomplete = json!({"type": "response.incomplete", "response": {"status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"}, "output": []}});
        assert!(stream.push(&incomplete.to_string()).unwrap().is_empty());
        let reply = stream.finish();
        assert_eq!(reply.cutoff, Some(Cutoff::TokenLimit));
        assert_eq!(reply.text, "half a rev");

        let mut stream = ReplyStream::new(EndpointApi::Responses);
        let body = json!({"object": "response", "status": "incomplete", "error": null,
            "incomplete_details": {"reason": "content_filter"},
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "half"}]}]});
        assert_eq!(
            texts(&stream.push_body(&body.to_string()).unwrap()),
            ["half"]
        );
        assert_eq!(stream.finish().cutoff, Some(Cutoff::ContentFilter));

        let mut stream = ReplyStream::new(EndpointApi::Responses);
        let incomplete = json!({"type": "response.incomplete", "response": {}});
        stream.push(&incomplete.to_string()).unwrap();
        assert_eq!(stream.finish().cutoff, Some(Cutoff::Other(None)));
    }

    #[test]
    fn responses_events_give_deltas_and_usage() {
        let mut stream = ReplyStream::new(EndpointApi::Responses);
        let created = json!({"type": "response.created", "response": {"model": "m", "output": []}});
        assert!(stream.push(&created.to_string()).unwrap().is_empty());
        let delta = json!({"type": "response.output_text.delta", "delta": "Hello"});
        assert_eq!(texts(&stream.push(&delta.to_string()).unwrap()), ["Hello"]);
        let done = json!({"type": "response.output_text.done", "text": "Hello"});
        assert!(stream.push(&done.to_string()).unwrap().is_empty());
        let completed = json!({"type": "response.completed", "response": {"model": "m",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "Hello"}]}],
            "usage": {"input_tokens": 5, "output_tokens": 1,
                      "input_tokens_details": {"cached_tokens": 2}}}});
        assert!(stream.push(&completed.to_string()).unwrap().is_empty());
        let reply = stream.finish();
        assert_eq!(reply.text, "Hello");
        assert_eq!(
            (reply.input_tokens, reply.output_tokens, reply.cached_tokens),
            (5, 1, 2)
        );
        assert!(reply.finished);
        assert_eq!(reply.cutoff, None);
    }

    #[test]
    fn length_and_filter_endings_cut_the_reply_off() {
        let ending = |api, payload: Value| {
            let mut stream = ReplyStream::new(api);
            stream.push_body(&payload.to_string()).unwrap();
            let reply = stream.finish();
            assert!(reply.finished, "{payload}");
            reply.cutoff
        };
        let chat = |reason: &str| json!({"model": "m", "choices": [{"message": {"content": "half"}, "finish_reason": reason}]});
        let chat_ending = |reason| ending(EndpointApi::ChatCompletions, chat(reason));
        assert_eq!(chat_ending("stop"), None);
        assert_eq!(chat_ending("tool_calls"), None);
        assert_eq!(chat_ending("length"), Some(Cutoff::TokenLimit));
        assert_eq!(chat_ending("content_filter"), Some(Cutoff::ContentFilter));

        let incomplete = |response: Value| {
            let event = json!({"type": "response.incomplete", "response": response});
            ending(EndpointApi::Responses, event)
        };
        let with_reason = |reason: &str| {
            json!({"model": "m", "status": "incomplete", "output": [],
                   "incomplete_details": {"reason": reason}})
        };
        assert_eq!(
            incomplete(with_reason("max_output_tokens")),
            Some(Cutoff::TokenLimit)
        );
        assert_eq!(
            incomplete(with_reason("content_filter")),
            Some(Cutoff::ContentFilter)
        );
        assert_eq!(
            incomplete(with_reason("server_shutdown")),
            Some(Cutoff::Other(Some("server_shutdown".to_owned())))
        );
        assert_eq!(incomplete(json!({})), Some(Cutoff::Other(None)));

        let body = json!({"object": "response", "model": "m", "error": null, "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"}, "output": []});
        assert_eq!(
            ending(EndpointApi::Responses, body),
            Some(Cutoff::TokenLimit)
        );
        let completed = json!({"type": "response.completed", "response": {"model": "m",
            "status": "completed", "incomplete_details": null, "output": []}});
        assert_eq!(ending(EndpointApi::Responses, completed), None);
    }

    #[test]
    fn the_served_model_is_announced_once() {
        let chunk = |model: &str| {
            json!({"model": model, "choices": [{"delta": {"content": "x"}, "finish_reason": null}]})
                .to_string()
        };
        let mut stream = ReplyStream::new(EndpointApi::ChatCompletions);
        assert_eq!(stream.announce(), None);
        stream.push(&chunk("")).unwrap();
        assert_eq!(stream.announce(), None);
        stream.push(&chunk("vendor/routed")).unwrap();
        assert_eq!(stream.announce().as_deref(), Some("vendor/routed"));
        stream.push(&chunk("vendor/routed")).unwrap();
        assert_eq!(stream.announce(), None);

        let mut retry = stream.next_attempt();
        retry.push(&chunk("vendor/routed")).unwrap();
        assert_eq!(retry.announce(), None);
        assert_eq!(retry.finish().text, "x");
        let mut rerouted = stream.next_attempt();
        rerouted.push(&chunk("vendor/fallback")).unwrap();
        assert_eq!(rerouted.announce().as_deref(), Some("vendor/fallback"));

        let mut stream = ReplyStream::new(EndpointApi::Responses);
        let created = json!({"type": "response.created", "response": {"model": "m", "output": []}});
        stream.push(&created.to_string()).unwrap();
        assert_eq!(stream.announce().as_deref(), Some("m"));
    }

    #[test]
    fn whole_bodies_are_read_for_both_apis() {
        let mut stream = ReplyStream::new(EndpointApi::ChatCompletions);
        let body = json!({"object": "chat.completion", "model": "m",
            "choices": [{"message": {"role": "assistant", "content": "all"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1}});
        assert_eq!(
            texts(&stream.push_body(&body.to_string()).unwrap()),
            ["all"]
        );
        assert!(stream.finish().finished);
        let mut stream = ReplyStream::new(EndpointApi::Responses);
        let body = json!({"object": "response", "model": "m", "error": null,
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "all"}]}],
            "usage": {"input_tokens": 3, "output_tokens": 1}});
        assert_eq!(
            texts(&stream.push_body(&body.to_string()).unwrap()),
            ["all"]
        );
        assert_eq!(stream.finish().output_tokens, 1);
    }
}
