//! Model and effort, set by the profile's route and read back before any prompt goes out.
//! Refuse, don't guess: a model the agent rejects ends the session as unavailable, and one it
//! reports differently ends it before it starts.

use argonauts_core::ErrorCategory;
use argonauts_core::driver::DriverStop;
use argonauts_core::registry::acp::{EffortRoute, ModelRoute};
use serde_json::{Value, json};

use crate::driver::Session;
use crate::errors::{AUTH_REQUIRED, failed};
use crate::rpc::RpcError;

const MODEL: &str = "model";
const THOUGHT_LEVEL: &str = "thought_level";

impl Session {
    pub(crate) async fn choose_model(&mut self) -> Result<(), DriverStop> {
        let asked = self.start.model.clone();
        let reported = match (self.profile.acp.model, asked.as_deref()) {
            (ModelRoute::ConfigOption, Some(model)) => self
                .set_option(MODEL, model)
                .await
                .map_err(|error| unavailable(model, error))?,
            (ModelRoute::LegacySetModel, Some(model)) => {
                // `session/set_model` is in neither v1 schema, so it goes out as a raw request.
                let params = json!({ "sessionId": self.session_id(), "modelId": model });
                match self.call("session/set_model", params).await? {
                    // What the new session reported predates the switch, so only this answer
                    // can confirm it.
                    Ok(result) => reported_model(&result),
                    Err(error) => return Err(unavailable(model, SetError::Refused(error))),
                }
            }
            _ => reported_model(&self.opened),
        };
        if let (Some(asked), Some(reported)) = (&asked, &reported)
            && asked != reported
        {
            return Err(mismatch(asked, reported));
        }
        self.model = reported.or(asked);
        Ok(())
    }

    pub(crate) async fn choose_effort(&mut self) -> Result<(), DriverStop> {
        let Some(effort) = self.start.effort.clone() else {
            return Ok(());
        };
        if self.profile.acp.effort != EffortRoute::ConfigOption {
            return Ok(());
        }
        let reported = match self.set_option(THOUGHT_LEVEL, &effort).await {
            Ok(reported) => reported,
            Err(SetError::NoOption) => {
                return Err(failed(
                    ErrorCategory::InvalidRequest,
                    format!("the agent offers no {THOUGHT_LEVEL} option for the effort {effort}"),
                ));
            }
            Err(SetError::Refused(error)) => {
                return Err(failed(
                    auth_or(error.code, ErrorCategory::InvalidRequest),
                    format!("the agent refused the effort {effort}: {}", error.text()),
                ));
            }
            Err(SetError::Stopped(stop)) => return Err(stop),
        };
        match reported {
            Some(reported) if reported != effort => Err(failed(
                ErrorCategory::InvalidRequest,
                format!("asked for the effort {effort}, the agent reports {reported}"),
            )),
            _ => Ok(()),
        }
    }

    /// A `config_option_update` that moves the model away from the one read back.
    pub(crate) fn model_drift(&self, update: &Value) -> Option<DriverStop> {
        let expected = self.model.as_deref()?;
        let reported = option_value(update, MODEL)?;
        (reported != expected).then(|| mismatch(expected, &reported))
    }

    /// Sets a config option by category and returns the value the agent reports back.
    async fn set_option(
        &mut self,
        category: &str,
        value: &str,
    ) -> Result<Option<String>, SetError> {
        let Some(config_id) = option_id(&self.opened, category) else {
            return Err(SetError::NoOption);
        };
        let params =
            json!({ "sessionId": self.session_id(), "configId": config_id, "value": value });
        match self.call("session/set_config_option", params).await {
            Ok(Ok(result)) => Ok(option_value(&result, category)),
            Ok(Err(error)) => Err(SetError::Refused(error)),
            Err(stop) => Err(SetError::Stopped(stop)),
        }
    }
}

enum SetError {
    NoOption,
    Refused(RpcError),
    Stopped(DriverStop),
}

fn unavailable(model: &str, error: SetError) -> DriverStop {
    match error {
        SetError::NoOption => failed(
            ErrorCategory::ModelUnavailable,
            format!("the agent offers no model option, so it can't run {model}"),
        ),
        SetError::Refused(error) => failed(
            auth_or(error.code, ErrorCategory::ModelUnavailable),
            format!("the agent refused the model {model}: {}", error.text()),
        ),
        SetError::Stopped(stop) => stop,
    }
}

fn auth_or(code: i64, category: ErrorCategory) -> ErrorCategory {
    if code == AUTH_REQUIRED {
        ErrorCategory::Auth
    } else {
        category
    }
}

/// A start failure: the worker ends the session before any prompt goes out.
fn mismatch(asked: &str, reported: &str) -> DriverStop {
    failed(
        ErrorCategory::InvalidRequest,
        format!("asked for {asked}, the agent reports {reported}"),
    )
}

/// The model an answer reports, from its config options or the older `models` state.
fn reported_model(answer: &Value) -> Option<String> {
    option_value(answer, MODEL).or_else(|| {
        answer
            .pointer("/models/currentModelId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

fn option_of<'a>(answer: &'a Value, category: &str) -> Option<&'a Value> {
    answer
        .get("configOptions")
        .and_then(Value::as_array)?
        .iter()
        .find(|option| option.get("category").and_then(Value::as_str) == Some(category))
}

fn option_id(answer: &Value, category: &str) -> Option<String> {
    option_of(answer, category)?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn option_value(answer: &Value, category: &str) -> Option<String> {
    option_of(answer, category)?
        .get("currentValue")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use argonauts_core::AgentEvent;
    use argonauts_core::registry::acp::AcpProfile;

    use super::*;
    use crate::testing;

    fn with_models(current: &str) -> Value {
        json!({ "configOptions": [
            { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": current,
              "options": [{ "value": "qwen3-coder", "name": "Qwen3 Coder" }, { "value": "gpt-oss-20b", "name": "gpt-oss" }] },
            { "id": "effort", "name": "Effort", "category": "thought_level", "type": "select", "currentValue": "medium",
              "options": [{ "value": "low", "name": "Low" }, { "value": "medium", "name": "Medium" }] },
        ] })
    }

    #[tokio::test]
    async fn acp_model_read_back_mismatch_fails_session() {
        let config_route = AcpProfile {
            effort: EffortRoute::ConfigOption,
            ..AcpProfile::default()
        };
        let mut start = testing::start("/w");
        start.model = Some("qwen3-coder".into());
        start.effort = Some("low".into());

        // The agent takes the model and effort and reports them back, so the prompt goes out.
        let mut driven = testing::launch(testing::profile(config_route.clone()), start.clone());
        driven
            .agent
            .open(json!({}), with_models("gpt-oss-20b"))
            .await;
        let set = driven.agent.expect("session/set_config_option").await;
        assert_eq!(set["params"]["configId"], "model");
        assert_eq!(set["params"]["value"], "qwen3-coder");
        driven.agent.reply(&set, with_models("qwen3-coder")).await;
        let set = driven.agent.expect("session/set_config_option").await;
        assert_eq!(set["params"]["configId"], "effort");
        let mut answer = with_models("qwen3-coder");
        answer["configOptions"][1]["currentValue"] = "low".into();
        driven.agent.reply(&set, answer).await;
        let prompt = driven.agent.expect("session/prompt").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "end_turn" }))
            .await;
        let (end, events) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::EndTurn);
        assert!(events.contains(&AgentEvent::Init {
            session_id: Some(testing::SESSION.into()),
            model: Some("qwen3-coder".into()),
            tools: Vec::new(),
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        }));

        // It accepts, then reports another model: the session fails before any prompt.
        let mut driven = testing::launch(testing::profile(config_route.clone()), start.clone());
        driven
            .agent
            .open(json!({}), with_models("gpt-oss-20b"))
            .await;
        let set = driven.agent.expect("session/set_config_option").await;
        driven.agent.reply(&set, with_models("gpt-oss-20b")).await;
        let (end, _) = driven.finish().await;
        assert_eq!(
            end.stop,
            failed(
                ErrorCategory::InvalidRequest,
                "asked for qwen3-coder, the agent reports gpt-oss-20b"
            )
        );

        // It rejects the model: unavailable.
        let mut driven = testing::launch(testing::profile(config_route.clone()), start.clone());
        driven
            .agent
            .open(json!({}), with_models("gpt-oss-20b"))
            .await;
        let set = driven.agent.expect("session/set_config_option").await;
        driven
            .agent
            .fail(&set, -32602, "unknown model qwen3-coder")
            .await;
        let (end, _) = driven.finish().await;
        assert!(
            matches!(
                &end.stop,
                DriverStop::Failed {
                    category: ErrorCategory::ModelUnavailable,
                    ..
                }
            ),
            "{end:?}"
        );

        // The legacy route sends `session/set_model`, and reads back what the agent reports.
        let legacy = AcpProfile {
            model: ModelRoute::LegacySetModel,
            ..AcpProfile::default()
        };
        let mut driven = testing::launch(testing::profile(legacy.clone()), start.clone());
        driven
            .agent
            .open(
                json!({}),
                json!({ "models": { "currentModelId": "openrouter:gpt-5" } }),
            )
            .await;
        let set = driven.agent.expect("session/set_model").await;
        assert_eq!(set["params"]["modelId"], "qwen3-coder");
        driven
            .agent
            .reply(
                &set,
                json!({ "models": { "currentModelId": "openrouter:gpt-5" } }),
            )
            .await;
        let (end, _) = driven.finish().await;
        assert_eq!(
            end.stop,
            failed(
                ErrorCategory::InvalidRequest,
                "asked for qwen3-coder, the agent reports openrouter:gpt-5"
            )
        );

        // A launch-argument model is checked against what the new session reports.
        let launch = AcpProfile {
            model: ModelRoute::LaunchArg,
            ..AcpProfile::default()
        };
        let mut driven = testing::launch(testing::profile(launch), start.clone());
        driven
            .agent
            .open(json!({}), with_models("gpt-oss-20b"))
            .await;
        let (end, _) = driven.finish().await;
        assert!(
            matches!(&end.stop, DriverStop::Failed { message, .. } if message.contains("agent reports gpt-oss-20b"))
        );

        // A config update that moves the model mid-turn fails the session too.
        let mut driven = testing::launch(testing::profile(legacy), start);
        driven.agent.open(json!({}), json!({})).await;
        let set = driven.agent.expect("session/set_model").await;
        driven.agent.reply(&set, json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        let mut drift = with_models("gpt-oss-20b");
        drift["sessionUpdate"] = "config_option_update".into();
        driven.agent.update(drift).await;
        driven.agent.expect("session/cancel").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "cancelled" }))
            .await;
        let (end, _) = driven.finish().await;
        assert_eq!(
            end.stop,
            failed(
                ErrorCategory::InvalidRequest,
                "asked for qwen3-coder, the agent reports gpt-oss-20b"
            )
        );
    }
}
