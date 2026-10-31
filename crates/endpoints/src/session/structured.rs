//! Structured output, per `structured_output`: `json_schema` asks the API for the strict
//! schema, `json_object` for any JSON, and `prompt` puts the schema in the prompt. Each is
//! validated here; the step runner retries once on a mismatch.

use argonauts_core::runs::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::agents::{EndpointApi, StructuredMode};
use argonauts_core::workflows::answer_format;
use serde_json::{Value, json};

/// The name the schema goes by in the request. OpenAI allows letters, digits, `_`, and `-`.
const SCHEMA_NAME: &str = "output";

/// Where the request asks for JSON: the body key and its value. `None` for `prompt` mode.
pub(crate) fn response_format(
    api: EndpointApi,
    mode: StructuredMode,
    schema: &Value,
) -> Option<(&'static str, Value)> {
    let format = match mode {
        StructuredMode::Prompt => return None,
        StructuredMode::JsonObject => json!({"type": "json_object"}),
        // Strict mode wants every field required, an optional one allowed to be null, which
        // `read` leaves out again.
        StructuredMode::JsonSchema => {
            let schema = answer_format::strict(schema);
            match api {
                EndpointApi::ChatCompletions => json!({
                    "type": "json_schema",
                    "json_schema": {"name": SCHEMA_NAME, "strict": true, "schema": schema},
                }),
                EndpointApi::Responses => json!({
                    "type": "json_schema", "name": SCHEMA_NAME, "strict": true, "schema": schema,
                }),
            }
        }
    };
    Some(match api {
        EndpointApi::ChatCompletions => ("response_format", format),
        EndpointApi::Responses => ("text", json!({ "format": format })),
    })
}

/// The schema as a prompt part, for the modes where the API doesn't carry it. `json_object`
/// gets it too: the model needs the shape, and OpenAI refuses JSON mode unless the prompt says
/// "JSON".
pub(crate) fn schema_part(mode: StructuredMode, schema: &Value) -> Option<InputPart> {
    if mode == StructuredMode::JsonSchema {
        return None;
    }
    let pretty = serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
    let text = format!(
        "\n\nReply with only a JSON object that matches this JSON Schema, with no other text:\n\
         {pretty}\n"
    );
    Some(InputPart {
        kind: InputPartKind::Schema,
        label: "schema".to_owned(),
        approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
        text,
        source: None,
    })
}

/// The reply's JSON, read as an answer to `schema` as written, validated against it. The error
/// lists what didn't match.
pub(crate) fn read(schema: &Value, text: &str) -> Result<Value, String> {
    let validator =
        jsonschema::validator_for(schema).map_err(|err| format!("the schema is invalid: {err}"))?;
    let value =
        answer_format::json_in(text).ok_or_else(|| "the reply held no JSON object".to_owned())?;
    let value = answer_format::as_written(schema, value);
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|err| {
            let at = err.instance_path().to_string();
            if at.is_empty() {
                err.to_string()
            } else {
                format!("{at}: {err}")
            }
        })
        .collect();
    if errors.is_empty() {
        Ok(value)
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        json!({"type": "object", "additionalProperties": false, "required": ["verdict"],
               "properties": {"verdict": {"type": "string", "enum": ["pass", "fail"]}}})
    }

    /// A strict reply's null for a field the schema left optional reads as the field left out.
    #[test]
    fn a_null_for_an_optional_field_is_left_out() {
        let schema = json!({"type": "object", "required": ["verdict"],
                            "properties": {"verdict": {"type": "string"},
                                           "notes": {"type": "array", "items": {"type": "string"}}}});
        assert_eq!(
            read(&schema, r#"{"verdict": "pass", "notes": null}"#),
            Ok(json!({"verdict": "pass"}))
        );
    }

    #[test]
    fn replies_are_read_whole_fenced_or_from_prose() {
        let want = json!({"verdict": "pass"});
        assert_eq!(read(&schema(), r#" {"verdict":"pass"} "#), Ok(want.clone()));
        assert_eq!(
            read(&schema(), "```json\n{\"verdict\":\"pass\"}\n```"),
            Ok(want.clone())
        );
        let prose = r#"First {"draft": {"x": 1}} then {"verdict": "pass"} done."#;
        assert_eq!(read(&schema(), prose), Ok(want));
    }

    #[test]
    fn mismatches_and_missing_json_are_errors() {
        let err = read(&schema(), r#"{"verdict": "maybe"}"#).unwrap_err();
        assert!(err.contains("/verdict"), "{err}");
        assert_eq!(
            read(&schema(), "no json here"),
            Err("the reply held no JSON object".into())
        );
        assert!(
            read(&json!({"type": 5}), "{}")
                .unwrap_err()
                .contains("schema is invalid")
        );
    }

    #[test]
    fn each_mode_asks_for_json_its_own_way() {
        let chat = EndpointApi::ChatCompletions;
        let (key, format) = response_format(chat, StructuredMode::JsonSchema, &schema()).unwrap();
        assert_eq!(key, "response_format");
        assert_eq!(format["json_schema"]["strict"], json!(true));
        assert_eq!(format["json_schema"]["schema"], schema());
        let (key, format) = response_format(
            EndpointApi::Responses,
            StructuredMode::JsonSchema,
            &schema(),
        )
        .unwrap();
        assert_eq!(key, "text");
        assert_eq!(format["format"]["schema"], schema());
        let (_, format) = response_format(chat, StructuredMode::JsonObject, &schema()).unwrap();
        assert_eq!(format, json!({"type": "json_object"}));
        assert!(response_format(chat, StructuredMode::Prompt, &schema()).is_none());
        assert!(schema_part(StructuredMode::JsonSchema, &schema()).is_none());
        let part = schema_part(StructuredMode::Prompt, &schema()).unwrap();
        assert!(part.text.contains("\"verdict\"") && part.text.contains("JSON"));
    }
}
