//! The request: a system part for the role prompt and a user part for the rest, sent to
//! `chat/completions` or `responses` with streaming on, plus the key and headers read from
//! the environment variables the settings name.

use argonauts_core::events::session::InputPart;
use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::settings::agents::{EndpointApi, EndpointSettings, StructuredMode};
use serde_json::{Value, json};

use crate::structured;

/// The full URL for `settings.api` under `base_url`. The settings are the registry's merged
/// ones, so a known endpoint such as `ollama` already carries its address.
pub(crate) fn url(id: &str, settings: &EndpointSettings) -> Result<String, String> {
    let base = settings.base_url.as_deref().ok_or_else(|| {
        format!("the {id} endpoint has no base_url. Set one under [endpoints.{id}]")
    })?;
    let path = match settings.api {
        EndpointApi::ChatCompletions => "chat/completions",
        EndpointApi::Responses => "responses",
    };
    Ok(format!("{}/{path}", base.trim_end_matches('/')))
}

/// The reasoning effort a request carries, next to the level the session asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effort {
    Unset,
    Sent(String),
    /// The model doesn't list the asked level, so its nearest listed level goes instead.
    Mapped {
        asked: String,
        sent: String,
    },
    /// Nothing is listed for the model, so the field stays out.
    Dropped {
        asked: String,
    },
}

impl Effort {
    /// OpenAI-compatible APIs define only some of Argonauts Code's levels, and a model that doesn't
    /// reason rejects the field, so only a level the registry lists for the model goes out.
    pub fn choose(asked: Option<&str>, listed: &[String]) -> Self {
        let Some(asked) = asked else {
            return Self::Unset;
        };
        if listed.iter().any(|level| level == asked) {
            return Self::Sent(asked.to_owned());
        }
        match nearest_level(asked, listed) {
            Some(sent) => Self::Mapped {
                asked: asked.to_owned(),
                sent: sent.to_owned(),
            },
            None => Self::Dropped {
                asked: asked.to_owned(),
            },
        }
    }

    pub fn sent(&self) -> Option<&str> {
        match self {
            Self::Sent(level) | Self::Mapped { sent: level, .. } => Some(level),
            Self::Unset | Self::Dropped { .. } => None,
        }
    }

    /// How the launch record shows it, so a changed or dropped level is on record.
    pub fn launch_arg(&self) -> Option<String> {
        match self {
            Self::Unset => None,
            Self::Sent(level) => Some(format!("effort={level}")),
            Self::Mapped { asked, sent } => Some(format!(
                "effort={sent} in place of {asked}, which the model doesn't list"
            )),
            Self::Dropped { asked } => Some(format!(
                "effort {asked} left out, since the registry lists no effort levels for the model"
            )),
        }
    }
}

/// The listed level closest to `asked` in `EFFORT_LEVELS` order. A tie goes to the lower one.
fn nearest_level<'a>(asked: &str, listed: &'a [String]) -> Option<&'a str> {
    let rank = |level: &str| EFFORT_LEVELS.iter().position(|known| *known == level);
    let asked = rank(asked)?;
    listed
        .iter()
        .filter_map(|level| Some((rank(level)?, level.as_str())))
        .min_by_key(|&(at, _)| (at.abs_diff(asked), at))
        .map(|(_, level)| level)
}

/// The prompt split in two. Together they are the parts' text, byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prompt {
    /// The leading role-prompt parts.
    pub system: String,
    /// Everything from the first other part on.
    pub user: String,
}

pub(crate) fn split_prompt(parts: &[InputPart]) -> Prompt {
    let role = parts
        .iter()
        .take_while(|part| part.kind.instructs())
        .count();
    let join = |parts: &[InputPart]| parts.iter().map(|part| part.text.as_str()).collect();
    Prompt {
        system: join(&parts[..role]),
        user: join(&parts[role..]),
    }
}

/// What a request carries besides the prompt.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Options<'a> {
    pub api: EndpointApi,
    pub mode: StructuredMode,
    pub model: &'a str,
    pub schema: Option<&'a Value>,
    pub effort: Option<&'a str>,
}

/// The JSON body, streamed, with the output schema placed as `mode` says.
pub(crate) fn body(prompt: &Prompt, options: Options<'_>) -> Value {
    let mut body = match options.api {
        EndpointApi::ChatCompletions => {
            let mut messages = Vec::new();
            if !prompt.system.is_empty() {
                messages.push(json!({"role": "system", "content": prompt.system}));
            }
            messages.push(json!({"role": "user", "content": prompt.user}));
            json!({
                "model": options.model,
                "messages": messages,
                "stream": true,
                // Without this, streamed chat completions carry no usage.
                "stream_options": {"include_usage": true},
            })
        }
        EndpointApi::Responses => {
            let mut body = json!({"model": options.model, "input": prompt.user, "stream": true});
            if !prompt.system.is_empty() {
                body["instructions"] = json!(prompt.system);
            }
            body
        }
    };
    if let Some(effort) = options.effort {
        match options.api {
            EndpointApi::ChatCompletions => body["reasoning_effort"] = json!(effort),
            EndpointApi::Responses => body["reasoning"] = json!({ "effort": effort }),
        }
    }
    if let Some(schema) = options.schema
        && let Some((key, format)) = structured::response_format(options.api, options.mode, schema)
    {
        body[key] = format;
    }
    body
}

/// The headers to send, and the values that must never reach a log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Credentials {
    pub headers: Vec<(String, String)>,
    pub secrets: Vec<String>,
}

/// Why credentials couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MissingCredential {
    Key(String),
    Header(String),
}

/// The key comes only from `api_key_env`'s variable and header values only from `header_env`'s,
/// looked up in the session's environment first, then the process's.
pub(crate) fn credentials(
    id: &str,
    settings: &EndpointSettings,
    env: &[(String, String)],
) -> Result<Credentials, MissingCredential> {
    let lookup = |name: &str| {
        env.iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var(name).ok())
            .filter(|value| !value.is_empty())
    };
    let mut credentials = Credentials::default();
    if let Some(variable) = &settings.api_key_env {
        let key = lookup(variable).ok_or_else(|| {
            MissingCredential::Key(format!("set {variable} to the {id} endpoint's API key"))
        })?;
        credentials
            .headers
            .push(("authorization".to_owned(), format!("Bearer {key}")));
        credentials.secrets.push(key);
    }
    for (header, variable) in &settings.header_env {
        let value = lookup(variable).ok_or_else(|| {
            MissingCredential::Header(format!(
                "the {id} endpoint's {header} header comes from {variable}, which isn't set"
            ))
        })?;
        // Short values such as a version number would blank out ordinary text in the log.
        if value.len() >= 8 {
            credentials.secrets.push(value.clone());
        }
        credentials.headers.push((header.clone(), value));
    }
    Ok(credentials)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::InputPartKind;

    fn part(kind: InputPartKind, text: &str) -> InputPart {
        InputPart {
            kind,
            label: String::new(),
            text: text.to_owned(),
            approx_tokens: 0,
            source: None,
        }
    }

    #[test]
    fn the_url_needs_a_base_url() {
        let settings = EndpointSettings::default();
        let missing = url("ollama", &settings).unwrap_err();
        assert!(missing.contains("[endpoints.ollama]"), "{missing}");
        let settings = EndpointSettings {
            base_url: Some("http://host:1/v1/".into()),
            api: EndpointApi::Responses,
            ..EndpointSettings::default()
        };
        assert_eq!(
            url("mine", &settings).unwrap(),
            "http://host:1/v1/responses"
        );
    }

    fn levels(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_listed_effort_is_sent_as_asked() {
        let effort = Effort::choose(Some("high"), &levels(&["low", "medium", "high"]));
        assert_eq!(effort.sent(), Some("high"));
        assert_eq!(effort.launch_arg().as_deref(), Some("effort=high"));
        assert_eq!(Effort::choose(None, &levels(&["low"])), Effort::Unset);
        assert_eq!(Effort::Unset.launch_arg(), None);
    }

    #[test]
    fn an_unlisted_effort_becomes_the_nearest_listed_one() {
        let listed = levels(&["low", "medium", "high"]);
        let effort = Effort::choose(Some("max"), &listed);
        assert_eq!(
            effort,
            Effort::Mapped {
                asked: "max".into(),
                sent: "high".into()
            }
        );
        let record = effort.launch_arg().unwrap();
        assert!(
            record.contains("effort=high") && record.contains("max"),
            "{record}"
        );
        assert_eq!(Effort::choose(Some("minimal"), &listed).sent(), Some("low"));
        // Equally far from low and high, so the lower level wins.
        let gap = levels(&["low", "high"]);
        assert_eq!(Effort::choose(Some("medium"), &gap).sent(), Some("low"));
    }

    #[test]
    fn a_model_without_effort_levels_gets_no_effort_field() {
        let effort = Effort::choose(Some("max"), &[]);
        assert_eq!(
            effort,
            Effort::Dropped {
                asked: "max".into()
            }
        );
        assert_eq!(effort.sent(), None);
        assert!(effort.launch_arg().unwrap().contains("max"));
        let prompt = Prompt {
            system: String::new(),
            user: "hi".into(),
        };
        for api in [EndpointApi::ChatCompletions, EndpointApi::Responses] {
            let options = Options {
                api,
                mode: StructuredMode::Prompt,
                model: "llama3.2",
                schema: None,
                effort: effort.sent(),
            };
            let sent = body(&prompt, options);
            assert!(sent.get("reasoning_effort").is_none(), "{sent}");
            assert!(sent.get("reasoning").is_none(), "{sent}");
        }
    }

    #[test]
    fn the_role_prompt_is_the_system_part() {
        let parts = [
            part(InputPartKind::Prompt, "You review. "),
            part(InputPartKind::Task, "Task 1. "),
            part(InputPartKind::Prompt, "Late rule."),
        ];
        let prompt = split_prompt(&parts);
        assert_eq!(prompt.system, "You review. ");
        assert_eq!(prompt.user, "Task 1. Late rule.");
        let all: String = parts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(format!("{}{}", prompt.system, prompt.user), all);
    }

    #[test]
    fn bodies_stream_and_carry_the_schema() {
        let prompt = Prompt {
            system: "sys".into(),
            user: "hi".into(),
        };
        let schema = json!({"type": "object"});
        let options = Options {
            api: EndpointApi::ChatCompletions,
            mode: StructuredMode::JsonSchema,
            model: "llama3.2",
            schema: Some(&schema),
            effort: Some("low"),
        };
        let chat = body(&prompt, options);
        assert_eq!(
            chat["messages"][0],
            json!({"role": "system", "content": "sys"})
        );
        assert_eq!(chat["stream"], json!(true));
        assert_eq!(chat["reasoning_effort"], json!("low"));
        assert_eq!(chat["response_format"]["json_schema"]["schema"], schema);
        let responses = body(
            &prompt,
            Options {
                api: EndpointApi::Responses,
                effort: None,
                ..options
            },
        );
        assert_eq!(responses["instructions"], json!("sys"));
        assert_eq!(responses["input"], json!("hi"));
        assert_eq!(responses["text"]["format"]["type"], json!("json_schema"));
    }

    #[test]
    fn keys_and_headers_come_from_their_variables() {
        let settings = EndpointSettings {
            api_key_env: Some("ARGONAUTS_TEST_UNSET_KEY_VAR".into()),
            header_env: [("x-title".to_owned(), "ARGONAUTS_TEST_TITLE".to_owned())].into(),
            ..EndpointSettings::default()
        };
        assert!(matches!(
            credentials("or", &settings, &[]),
            Err(MissingCredential::Key(message)) if message.contains("ARGONAUTS_TEST_UNSET_KEY_VAR")
        ));
        let env = vec![
            (
                "ARGONAUTS_TEST_UNSET_KEY_VAR".to_owned(),
                "k-123456789".to_owned(),
            ),
            ("ARGONAUTS_TEST_TITLE".to_owned(), "app".to_owned()),
        ];
        let found = credentials("or", &settings, &env).unwrap();
        assert_eq!(
            found.headers,
            [
                ("authorization".to_owned(), "Bearer k-123456789".to_owned()),
                ("x-title".to_owned(), "app".to_owned())
            ]
        );
        assert_eq!(found.secrets, ["k-123456789"]);
    }
}
