//! The consultant, and the main agent answering an agent in your place in autopilot.

pub const CONSULTANT: &str = argonauts_core::template!("prompts/run/consultant.md");
pub const FOR_YOU: &str = argonauts_core::template!("prompts/run/answer-for-you.md");
/// What every agent hears up front while a run decides without you.
pub const DECIDE_FIRST: &str = argonauts_core::template!("prompts/run/decide-first.md");

/// The word on questions for one session: the template, then where its answer keeps what it
/// assumed and what it couldn't decide, when its answer has those fields. A session is never
/// told about a field its answer doesn't have.
pub fn decide_first(template: &str, answer: Option<&serde_json::Value>) -> String {
    let has = |field: &str| {
        answer
            .and_then(|schema| schema.get("properties"))
            .is_some_and(|properties| properties.get(field).is_some())
    };
    let mut text = template.trim_end().to_owned();
    // A template of yours that names the field already says it.
    let says = |field: &str| template.contains(&format!("`{field}`"));
    if has("concerns") && !says("concerns") {
        text.push_str(
            " List each thing you assumed in `concerns`, on a line starting with \"Assumed:\".",
        );
    }
    if has("questions") && !says("questions") {
        text.push_str(
            " If you can't do the work at all without an answer, put the question in \
             `questions` and finish what you can.",
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_answer_is_told_only_about_fields_it_has() {
        let review = json!({"properties": {"accept": {}, "findings": {}, "summary": {}}});
        let said = decide_first(DECIDE_FIRST, Some(&review));
        assert!(
            !said.contains("concerns") && !said.contains("questions"),
            "{said}"
        );
        let report = json!({"properties": {"status": {}, "concerns": {}, "questions": {}}});
        let said = decide_first(DECIDE_FIRST, Some(&report));
        assert!(said.contains("in `concerns`, on a line starting with \"Assumed:\""));
        assert!(said.contains("put the question in `questions`"));
        assert_eq!(decide_first(DECIDE_FIRST, None), DECIDE_FIRST.trim_end());
    }
}
