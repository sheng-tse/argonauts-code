//! The setup form as the main session fills it, and the checks it goes through before you see
//! it: every setting the way a settings file is checked, each role, prompt and mod by a name
//! Argonauts Code knows, and a place to keep it only when your own words named one.

use argonauts_core::base::names::ModelRef;
use argonauts_core::prompts::prompt_files::{RoleFileMode, Template};
use argonauts_core::prompts::prompt_parts::Readers;
use argonauts_core::workflows::required::{RequiredStep, role_named, same_name, when_problem};
use argonauts_registry::SettingsError;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::controller::Ctx;
use crate::controller::instructions::said_in;
use crate::controller::settings::{check_known, workflow_list};
use crate::prompts::catalog;

/// What the session sends, as `argonauts_mcp::session::setup` describes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Form {
    asked: String,
    #[serde(default)]
    settings: Map<String, Value>,
    #[serde(default)]
    roles: Vec<RoleText>,
    #[serde(default)]
    prompts: Vec<PromptText>,
    #[serde(default)]
    mods: Vec<ModText>,
    #[serde(default)]
    steps: Vec<StepText>,
    #[serde(default)]
    keep: Option<String>,
    #[serde(default)]
    keep_says: String,
    #[serde(default)]
    request_too: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleText {
    role: String,
    text: String,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptText {
    prompt: String,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModText {
    name: String,
    toml: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepText {
    name: String,
    role: String,
    #[serde(default)]
    does: Option<String>,
    #[serde(default)]
    when: Option<String>,
}

/// A setup that passed every check: what was asked, and what it would write.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Proposal {
    pub asked: String,
    /// Keys and TOML values, as `/config` writes them.
    pub settings: Vec<(String, String)>,
    pub roles: Vec<RoleChange>,
    pub prompts: Vec<PromptChange>,
    pub mods: Vec<ModChange>,
    /// Steps every workflow must contain, for `workflows/always.toml`.
    pub steps: Vec<RequiredStep>,
    /// Where your own words said to keep it. `None` asks you on the question.
    pub keep: Option<Place>,
    /// Your message asks for work as well, so the change may hold for that request alone.
    pub request_too: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct RoleChange {
    pub readers: Readers,
    pub mode: RoleFileMode,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PromptChange {
    pub template: Template,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ModChange {
    pub name: String,
    pub text: String,
}

/// Where a setup applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    /// The workflow the same message starts, and nothing after it.
    Request,
    /// Until you quit, as a session setting.
    Conversation,
    /// `.argonauts/` in this repository.
    Repository,
    /// `~/.argonauts/`.
    AllRepositories,
}

impl Place {
    pub const ALL: [Self; 4] = [
        Self::Request,
        Self::Conversation,
        Self::Repository,
        Self::AllRepositories,
    ];

    /// Its name on the form and in a question's answer.
    pub fn value(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Conversation => "conversation",
            Self::Repository => "repository",
            Self::AllRepositories => "all_repositories",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|place| place.value() == value)
    }

    /// The place the user's own words name, as the tool's schema reads them: all their
    /// repositories, then this one, then "save that", then "from now on", then this request.
    /// `None` when they name no place, and then the question asks.
    pub fn named_in(says: &str) -> Option<Self> {
        let words: Vec<String> = says
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect();
        let has = |phrase: &str| {
            let phrase: Vec<&str> = phrase.split(' ').collect();
            words
                .windows(phrase.len())
                .any(|window| window.iter().zip(&phrase).all(|(word, want)| word == want))
        };
        let any = |phrases: &[&str]| phrases.iter().any(|phrase| has(phrase));
        if any(&[
            "all my repos",
            "all my repositories",
            "all my projects",
            "all repos",
            "all repositories",
            "every repo",
            "every repository",
            "every project",
            "everywhere",
        ]) {
            Some(Self::AllRepositories)
        } else if any(&[
            "this repo",
            "this repository",
            "this project",
            "save that",
            "save this",
            "save it",
        ]) {
            Some(Self::Repository)
        } else if any(&[
            "from now on",
            "this conversation",
            "this session",
            "until i quit",
        ]) {
            Some(Self::Conversation)
        } else if any(&[
            "for this one",
            "this request",
            "just this",
            "only this",
            "this time",
        ]) {
            Some(Self::Request)
        } else {
            None
        }
    }
}

impl Proposal {
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty()
            && self.roles.is_empty()
            && self.prompts.is_empty()
            && self.mods.is_empty()
            && self.steps.is_empty()
    }
}

/// Checks the form against your message `said`. Every problem comes back at once, for the
/// model to fix before you see anything.
pub(super) fn check(ctx: &Ctx, arguments: &Value, said: &str) -> Result<Proposal, String> {
    let form: Form = serde_json::from_value(arguments.clone())
        .map_err(|err| format!("The form doesn't read: {err}."))?;
    let mut problems = Vec::new();
    let settings = settings(ctx, form.settings, &mut problems);
    let roles = form
        .roles
        .into_iter()
        .filter_map(|role| role_change(role, &mut problems))
        .collect();
    let prompts = form
        .prompts
        .into_iter()
        .filter_map(|prompt| prompt_change(prompt, &mut problems))
        .collect();
    let mods = form
        .mods
        .into_iter()
        .filter_map(|file| mod_change(file, &mut problems))
        .collect();
    let steps = required_steps(form.steps, &mut problems);
    // The place stands only when the user's own words, quoted from their message, name that
    // very place. Anything else opens the question, which asks.
    let keep = form.keep.as_deref().and_then(Place::parse).filter(|place| {
        said_in(said, &form.keep_says) && Place::named_in(&form.keep_says) == Some(*place)
    });
    let proposal = Proposal {
        asked: form.asked.trim().to_owned(),
        settings,
        roles,
        prompts,
        mods,
        steps,
        keep,
        request_too: form.request_too || keep == Some(Place::Request),
    };
    if proposal.asked.is_empty() {
        problems.push("asked: say what the user asked for, in their words.".to_owned());
    }
    if problems.is_empty() && proposal.is_empty() {
        problems.push(
            "Nothing to set: fill settings, roles, prompts, mods or steps with what the user \
             asked to change."
                .to_owned(),
        );
    }
    if problems.is_empty() {
        return Ok(proposal);
    }
    let mut message = String::from("Nothing was shown to the user. Fix these and call again:");
    for problem in problems {
        message.push_str("\n- ");
        message.push_str(&problem);
    }
    Err(message)
}

/// Each setting as `/config` would take it, then all of them as one file would be checked.
fn settings(
    ctx: &Ctx,
    given: Map<String, Value>,
    problems: &mut Vec<String>,
) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (key, value) in given {
        let Some(text) = toml_text(&value) else {
            problems.push(format!(
                "{key}: null isn't a value. Leave the key out to keep it as it is."
            ));
            continue;
        };
        match check_known(ctx, &key, &text) {
            Ok(text) => pairs.push((key, text)),
            Err(err) => problems.push(format!("{err:#}")),
        }
    }
    if !pairs.is_empty() {
        let workflows: Vec<String> = workflow_list(ctx)
            .into_iter()
            .map(|info| info.name)
            .collect();
        if let Err(err) = argonauts_registry::check_settings(&pairs, &workflows) {
            problems.push(match err {
                SettingsError::Invalid { message, .. } => message,
                other => other.to_string(),
            });
        }
    }
    // A crew list naming an agent that can't take part goes back to the model, never into a
    // question.
    for (key, text) in &pairs {
        if key.starts_with("crew.") {
            let named = agents_in(text);
            problems.extend(argonauts_registry::crew_list_problems(
                ctx.settings(),
                key,
                &named,
            ));
        }
    }
    pairs
}

/// The agents a `crew.*` value names: one, or a list. `main` is the main agent, not one of
/// the list.
fn agents_in(text: &str) -> Vec<ModelRef> {
    let value: Option<toml::Value> = toml::from_str::<toml::Table>(&format!("v = {text}"))
        .ok()
        .and_then(|mut table| table.remove("v"));
    let names: Vec<String> = match value {
        Some(toml::Value::String(one)) => vec![one],
        Some(toml::Value::Array(list)) => list
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    names
        .iter()
        .filter(|name| *name != "main")
        .filter_map(|name| name.parse().ok())
        .collect()
}

/// A JSON value as TOML text. Null has no TOML form.
fn toml_text(value: &Value) -> Option<String> {
    let value: toml::Value = serde_json::from_value(value.clone()).ok()?;
    Some(value.to_string())
}

fn role_change(role: RoleText, problems: &mut Vec<String>) -> Option<RoleChange> {
    let name = role.role.trim();
    let Some(readers) = Readers::of_file(name) else {
        problems.push(format!(
            "roles: {name:?} isn't a role. Use main, orchestrator, worker, reviewer or judge, \
             or a finer role such as critic."
        ));
        return None;
    };
    let mode = match role.mode.as_deref().map(str::trim) {
        None | Some("add") => RoleFileMode::Add,
        Some("replace") => RoleFileMode::Replace,
        Some(other) => {
            problems.push(format!(
                "roles: {name}'s mode is add or replace, not {other:?}."
            ));
            return None;
        }
    };
    let text = role.text.trim().to_owned();
    if text.is_empty() {
        problems.push(format!("roles: {name}'s text is empty."));
        return None;
    }
    Some(RoleChange {
        readers,
        mode,
        text,
    })
}

fn prompt_change(prompt: PromptText, problems: &mut Vec<String>) -> Option<PromptChange> {
    let id = prompt.prompt.trim().trim_end_matches(".md");
    let Some(template) = catalog::find(id) else {
        problems.push(format!(
            "prompts: there's no prompt named {id:?}. Its id is as /prompts lists it, such as \
             review/review or frames/reviewer."
        ));
        return None;
    };
    if prompt.text.trim().is_empty() {
        problems.push(format!("prompts: {id}'s text is empty."));
        return None;
    }
    Some(PromptChange {
        template,
        text: prompt.text,
    })
}

fn mod_change(file: ModText, problems: &mut Vec<String>) -> Option<ModChange> {
    let name = file.name.trim().trim_end_matches(".toml");
    match check_mod(name, &file.toml) {
        Ok(()) => Some(ModChange {
            name: name.to_owned(),
            text: file.toml,
        }),
        Err(problem) => {
            problems.push(format!("mods: {problem}"));
            None
        }
    }
}

/// Each step as `always.toml` would take it: a name, one of the crew's roles, and a place.
fn required_steps(given: Vec<StepText>, problems: &mut Vec<String>) -> Vec<RequiredStep> {
    let mut steps: Vec<RequiredStep> = Vec::new();
    for step in given {
        let name = step.name.trim().to_owned();
        let mut wrong = Vec::new();
        if name.is_empty() {
            wrong.push("a step needs a name".to_owned());
        } else if steps.iter().any(|known| same_name(&known.name, &name)) {
            wrong.push(format!("{name:?} is listed twice"));
        }
        let role = role_named(&step.role);
        if role.is_none() {
            wrong.push(format!(
                "{name:?}: role {:?} is none of main, orchestrator, worker, reviewer, judge",
                step.role
            ));
        }
        let when = step
            .when
            .map(|when| when.trim().to_owned())
            .filter(|when| !when.is_empty());
        if let Some(problem) = when.as_deref().and_then(when_problem) {
            wrong.push(format!("{name:?}: {problem}"));
        }
        match role {
            Some(role) if wrong.is_empty() => steps.push(RequiredStep {
                name,
                role,
                does: step
                    .does
                    .map(|does| does.trim().to_owned())
                    .filter(|does| !does.is_empty()),
                when,
            }),
            _ => problems.extend(wrong.into_iter().map(|problem| format!("steps: {problem}"))),
        }
    }
    steps
}

/// A mod as `/mods` takes one: a name a mod can have, and a file the mods' own checks pass,
/// so a wrong one goes back to the session before you see a question.
fn check_mod(name: &str, text: &str) -> Result<(), String> {
    use argonauts_core::settings::mods;

    if !mods::is_mod_name(name) {
        return Err(format!(
            "{name:?} isn't a mod name. Use lowercase letters, digits, - and _, starting with a \
             letter."
        ));
    }
    match mods::check::problems(name, text).as_slice() {
        [] => Ok(()),
        problems => Err(format!("{name}: {}", problems.join("; "))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::controller::base::test_support::{CliOptions, TestCtx};

    fn ctx() -> TestCtx {
        TestCtx::new(CliOptions::default()).0
    }

    const SAID: &str = "reviewers should treat a missing test as a blocker. never spend more \
                        than $5 a day, save that for this repository";

    #[test]
    fn an_agent_that_cant_take_part_goes_back_to_the_model() {
        let test = ctx();
        let form = json!({"asked": "x", "settings": {"crew.reviewer": ["gemini"]}});
        let problems = check(&test.ctx, &form, SAID).expect_err("gemini isn't an agent here");
        assert!(problems.contains("crew.reviewer"), "{problems}");
        assert!(problems.contains("gemini"), "{problems}");
        let good = json!({"asked": "x", "settings": {"crew.reviewer": ["codex/gpt-5.6"]}});
        assert!(check(&test.ctx, &good, SAID).is_ok());
    }

    #[test]
    fn a_valid_form_becomes_a_proposal() {
        let test = ctx();
        let form = json!({
            "asked": "Reviewers treat a missing test as a blocker; no more than $5 a day.",
            "settings": {"ui.theme": "light", "agents.codex.effort": "high"},
            "roles": [{"role": "reviewer", "text": "Treat a missing test as a required change."}],
            "prompts": [{"prompt": "review/review", "text": "Review it."}],
            "mods": [{"name": "ci", "toml": "title = \"CI\"\nbody = \"text\"\n[data]\nsource = \"spend\"\n"}]
        });
        let proposal = check(&test.ctx, &form, SAID).expect("a valid setup");
        assert_eq!(
            proposal.settings,
            [
                ("ui.theme".to_owned(), "\"light\"".to_owned()),
                ("agents.codex.effort".to_owned(), "\"high\"".to_owned()),
            ],
            "in the form's order"
        );
        assert_eq!(proposal.roles[0].readers.as_str(), "reviewer");
        assert_eq!(proposal.roles[0].mode, RoleFileMode::Add);
        assert_eq!(proposal.prompts[0].template.id, "review/review");
        assert_eq!(proposal.mods[0].name, "ci");
        assert_eq!(proposal.keep, None, "nothing said where");
        assert!(!proposal.request_too);
    }

    /// Checks a run had for itself alone are kept when you say so, as `checks.run` with the
    /// same commands.
    #[test]
    fn checks_kept_from_a_run_are_one_setting() {
        let test = ctx();
        let form = json!({
            "asked": "keep these checks",
            "settings": {
                "checks.run": [{"name": "tests", "command": ["python3", "-m", "unittest", "-q"]}]
            }
        });
        let proposal = check(&test.ctx, &form, "keep these checks").expect("a valid setup");
        let (key, value) = &proposal.settings[0];
        assert_eq!(key, "checks.run");
        assert!(value.contains("unittest"), "{value}");
    }

    #[test]
    fn every_problem_goes_back_to_the_model_at_once() {
        let test = ctx();
        let form = json!({
            "asked": "Set things up",
            "settings": {"ui.theme": "plaid", "limits.nonsense": 3, "ui.accent": null},
            "roles": [{"role": "testers", "text": "Test."}, {"role": "judge", "text": " "}],
            "prompts": [{"prompt": "review/nothing", "text": "x"}],
            "mods": [{"name": "my mod", "toml": "x"}, {"name": "ci", "toml": "title = \"CI\"\n"}]
        });
        let err = check(&test.ctx, &form, SAID).unwrap_err();
        assert!(err.starts_with("Nothing was shown to the user."), "{err}");
        for expected in [
            "ui.theme",
            "limits.nonsense",
            "ui.accent: null",
            "\"testers\" isn't a role",
            "judge's text is empty",
            "no prompt named \"review/nothing\"",
            "\"my mod\" isn't a mod name",
            "ci: it needs a body",
        ] {
            assert!(err.contains(expected), "{expected} in {err}");
        }
        let odd = check(&test.ctx, &json!({"asked": "x", "colour": "red"}), SAID).unwrap_err();
        assert!(odd.contains("colour"), "{odd}");
        let empty = check(&test.ctx, &json!({"asked": "x"}), SAID).unwrap_err();
        assert!(empty.contains("Nothing to set"), "{empty}");
    }

    /// The session's server lists the form; a field it lists and this file refuses, or the
    /// other way round, would fail only when a model first filled it.
    #[test]
    fn the_form_the_session_lists_is_the_form_read_here() {
        use argonauts_mcp::session::setup;
        let schema = setup::input_schema();
        let listed: Vec<&str> = schema["properties"]
            .as_object()
            .expect("fields")
            .keys()
            .map(String::as_str)
            .collect();
        let test = ctx();
        let mut every = json!({
            "asked": "x",
            "settings": {"ui.theme": "light"},
            "roles": [], "prompts": [], "mods": [],
            "steps": [{"name": "Benchmark", "role": "worker", "does": "Run it.", "when": "last"}],
            "keep": "ask", "keep_says": "", "request_too": false
        });
        let filled: Vec<&str> = every
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(listed, filled);
        check(&test.ctx, &every, SAID).expect("every field reads");
        for keep in setup::KEEP {
            every["keep"] = json!(keep);
            assert!(keep == "ask" || Place::parse(keep).is_some(), "{keep}");
            check(&test.ctx, &every, SAID).expect("every place reads");
        }
    }

    #[test]
    fn a_wrong_type_is_caught_as_a_file_would_be() {
        let test = ctx();
        let form = json!({"asked": "x", "settings": {"ui.ambiguous_width": "wide"}});
        let err = check(&test.ctx, &form, SAID).unwrap_err();
        assert!(err.contains("ui.ambiguous_width"), "{err}");
    }

    #[test]
    fn a_place_is_kept_only_when_your_own_words_named_it() {
        let test = ctx();
        let form = |keep: &str, says: &str| json!({"asked": "x", "settings": {"ui.theme": "light"}, "keep": keep, "keep_says": says});
        let said = check(
            &test.ctx,
            &form("repository", "save that for this repository"),
            SAID,
        );
        assert_eq!(said.expect("valid").keep, Some(Place::Repository));
        let made_up = check(
            &test.ctx,
            &form("all_repositories", "for all my repositories"),
            SAID,
        );
        assert_eq!(made_up.expect("valid").keep, None, "not in the message");
        let one_word = check(&test.ctx, &form("repository", "save"), SAID);
        assert_eq!(one_word.expect("valid").keep, None, "two words at least");
        let asked = check(&test.ctx, &form("ask", "save that"), SAID);
        assert_eq!(asked.expect("valid").keep, None);
        // "save that" names the repository, so a model that says request gets the question.
        let wrong_place = check(
            &test.ctx,
            &form("request", "save that"),
            "save that, and add due dates",
        )
        .expect("valid");
        assert_eq!(wrong_place.keep, None);
        // Two words of the message that name no place are no answer either.
        let any_words = check(&test.ctx, &form("all_repositories", "a missing"), SAID);
        assert_eq!(any_words.expect("valid").keep, None);
        let for_this = check(
            &test.ctx,
            &form("request", "for this one"),
            "use opus for reviewers for this one, and add due dates",
        )
        .expect("valid");
        assert_eq!(for_this.keep, Some(Place::Request));
        assert!(for_this.request_too);
        let everywhere = check(
            &test.ctx,
            &form("all_repositories", "from now on in all my repos"),
            "use opus for reviewers from now on in all my repos",
        )
        .expect("valid");
        assert_eq!(everywhere.keep, Some(Place::AllRepositories));
    }
}
