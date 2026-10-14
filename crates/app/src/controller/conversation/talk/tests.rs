//! The setup tool from the form to the files: the question, each place, Not now, and a place your
//! own words named.

use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind};
use argonauts_core::asking::AskReply;
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::names::TurnId;
use argonauts_core::settings::SettingScope;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::{Call, ConversationState, setup};
use crate::controller::Effect;
use crate::controller::test_support::{CliOptions, TestCtx};

const SAID: &str = "reviewers should treat a missing test as a blocker. never spend more than \
                    $5 a day.";

struct Rig {
    test: TestCtx,
    conversation: ConversationState,
}

impl Rig {
    fn new() -> Self {
        Self::with(CliOptions::default())
    }

    /// With `config` as your own settings file.
    fn with_config(config: &'static str) -> Self {
        Self::with(CliOptions {
            prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
                std::fs::write(repo.global_config(), config).unwrap();
            })),
            ..CliOptions::default()
        })
    }

    fn with(options: CliOptions) -> Self {
        let (test, _) = TestCtx::new(options);
        let conversation = ConversationState::new(&test.ctx).expect("a conversation");
        Self { test, conversation }
    }

    /// Calls the tool from turn 1, as the session would, and hands back the call's answer.
    fn call(&mut self, arguments: Value, said: &str) -> (Vec<Effect>, oneshot::Receiver<AskReply>) {
        let (reply, answered) = oneshot::channel();
        let call = Call {
            turn: TurnId::numbered(1),
            arguments,
            reply,
        };
        let effects = setup::propose(&mut self.test.ctx, &mut self.conversation, call, said);
        (effects, answered)
    }

    fn events(&mut self) -> Vec<AppEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.test.events.try_recv() {
            events.push(event);
        }
        events
    }

    fn question(&mut self) -> AppChoice {
        self.events()
            .into_iter()
            .find_map(|event| match event {
                AppEvent::Choice { choice } if choice.kind == ChoiceKind::Setup => Some(choice),
                _ => None,
            })
            .expect("the setup's question")
    }

    fn pick(&mut self, question: &AppChoice, value: &str) -> Vec<Effect> {
        let action = AnswerAction::Choose {
            value: value.to_owned(),
        };
        super::answer(
            &mut self.test.ctx,
            &mut self.conversation,
            question.id.clone(),
            action,
        )
        .expect("an answer")
    }
}

fn the_setup() -> Value {
    json!({
        "asked": "Reviewers treat a missing test as a blocker; never more than $5 a day.",
        "settings": {"agents.codex.effort": "high", "ui.theme": "light"},
        "roles": [{"role": "reviewer", "text": "Treat a missing test as a required change."}]
    })
}

fn said(answered: &mut oneshot::Receiver<AskReply>) -> AskReply {
    answered.try_recv().expect("the call was answered")
}

fn saved(effects: &[Effect]) -> Vec<(Vec<(String, String)>, SettingScope)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SaveSettings { pairs, scope }
            | Effect::SaveConfirmedSettings { pairs, scope } => Some((pairs.clone(), *scope)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_valid_setup_is_one_question_and_nothing_is_written_yet() {
    let mut rig = Rig::new();
    let (effects, mut answered) = rig.call(the_setup(), SAID);
    assert!(effects.is_empty());
    assert!(answered.try_recv().is_err(), "the call waits on your pick");
    let question = rig.question();
    assert_eq!(question.title, "Your setup");
    let table: Vec<&str> = question
        .body
        .iter()
        .take_while(|row| !row.is_empty())
        .map(String::as_str)
        .collect();
    assert_eq!(
        table,
        [
            "reviewers     \tadds to\t roles/reviewer.md   \"Treat a missing test as a required change.\"",
            "agents        codex.effort = \"high\"",
            "ui            theme = \"light\"",
        ]
    );
    let labels: Vec<&str> = question.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "This conversation",
            "Save for this repository",
            "Save for all my repositories",
            "Not now",
            "Show what would be written",
        ]
    );
    assert!(question.options[0].recommended);
    assert_eq!(
        question.options[1].detail.as_deref(),
        Some("writes 2 files in .argonauts/ · d shows the diff")
    );
    assert_eq!(question.options[4].action, AnswerAction::ShowDiff);
    let shown = question.body.join("\n");
    assert!(shown.contains("You asked: Reviewers treat"), "{shown}");
    assert!(shown.contains(".argonauts/config.toml"), "{shown}");
    assert!(shown.contains(".argonauts/roles/reviewer.md"), "{shown}");
    assert!(!rig.test.env.project_config().exists());
}

#[test]
fn the_crew_reads_as_the_screen_names_agents_in_the_crews_order() {
    let mut rig = Rig::new();
    let setup = json!({
        "asked": "Fable plans, codex builds, and the critic is picked from the crew.",
        "settings": {
            "roles.worker.models": ["codex/gpt-5.6"],
            "roles.critic.models": [],
            "roles.orchestrator.models": ["claude-code/claude-fable-5-1"],
            "ui.theme": "light"
        }
    });
    let (_, _answered) = rig.call(setup, SAID);
    let question = rig.question();
    let table: Vec<&str> = question
        .body
        .iter()
        .take_while(|row| !row.is_empty())
        .map(String::as_str)
        .collect();
    assert_eq!(
        table,
        [
            "crew          orchestrator   Fable 5.1 on Claude Code",
            "              workers        GPT 5.6 on Codex",
            "              critic         picked from the crew",
            "ui            theme = \"light\"",
        ]
    );
    let shown = question.body.join("\n");
    assert!(
        shown.contains("roles.orchestrator.models = [\"claude-code/claude-fable-5-1\"]"),
        "what would be written keeps the ids: {shown}"
    );
}

/// A setup said in words: the crew as the screen names agents, how the
/// workers keep each task, the judge that follows the reviewers, the reviewers' file, then the
/// limit with what happens when it's reached.
#[test]
fn a_crew_said_in_words_reads_with_who_follows_whom_and_how_each_task_is_kept() {
    // Your crew names reviewers and no judge, so the judge follows them.
    let mut rig = Rig::with_config("[crew]\nreviewer = [\"codex\"]\n");
    let setup = json!({
        "asked": "I want fable to plan, codex to build using its own goal, and astra to review.",
        "settings": {
            "crew.orchestrator": ["claude-code/claude-fable-5-1"],
            "crew.worker": ["codex/gpt-5.6"],
            "roles.worker.goal": "cli",
            "crew.reviewer": ["codex/gpt-6-astra"],
            "limits.spend_per_day": 5.0
        },
        "roles": [{"role": "reviewer", "text": "Treat a missing test as a required change."}]
    });
    let (_, _answered) = rig.call(setup, SAID);
    let question = rig.question();
    let table: Vec<&str> = question
        .body
        .iter()
        .take_while(|row| !row.is_empty())
        .map(String::as_str)
        .collect();
    assert_eq!(
        table,
        [
            "crew          orchestrator   Fable 5.1 on Claude Code",
            "              workers        GPT 5.6 on Codex       \teach task as Codex's own goal",
            "              reviewers      GPT 6 Astra on Codex",
            "              judge          GPT 6 Astra on Codex   \tfollows the reviewers",
            "reviewers     \tadds to\t roles/reviewer.md   \"Treat a missing test as a required change.\"",
            "limits        spend_per_day = 5.0                   \twhen reached: fallback",
        ]
    );
}

#[test]
fn saving_for_the_repository_writes_its_files() {
    let mut rig = Rig::new();
    let roles = rig.test.repo.root().join(".argonauts/roles");
    std::fs::create_dir_all(&roles).unwrap();
    std::fs::write(
        roles.join("reviewer.md"),
        "---\nmode: add\n---\nKeep reviews short.\n",
    )
    .unwrap();
    let mut setup = the_setup();
    setup["prompts"] = json!([{"prompt": "review/review", "text": "Review it as I would."}]);
    // A mod is saved only once it reads as one: a body and the data it draws.
    const CI: &str = "title = \"CI\"\nbody = \"text\"\n[data]\nsource = \"spend\"\n";
    setup["mods"] = json!([{"name": "ci", "toml": CI}]);
    let (_, mut answered) = rig.call(setup, SAID);
    let question = rig.question();
    assert_eq!(
        question.options[1].detail.as_deref(),
        Some("writes 4 files in .argonauts/ · d shows the diff")
    );
    let effects = rig.pick(&question, "repository");
    assert_eq!(
        saved(&effects),
        [(
            vec![
                ("agents.codex.effort".to_owned(), "\"high\"".to_owned()),
                ("ui.theme".to_owned(), "\"light\"".to_owned()),
            ],
            SettingScope::Project
        )]
    );
    let reviewers = std::fs::read_to_string(roles.join("reviewer.md")).unwrap();
    assert_eq!(
        reviewers,
        "---\nmode: add\n---\nKeep reviews short.\n\nTreat a missing test as a required change.\n",
        "your file keeps what it said"
    );
    let prompt = rig
        .test
        .repo
        .root()
        .join(".argonauts/prompts/review/review.md");
    assert_eq!(
        std::fs::read_to_string(prompt).unwrap(),
        "Review it as I would."
    );
    let ci = rig.test.repo.root().join(".argonauts/mods/ci.toml");
    assert_eq!(std::fs::read_to_string(ci).unwrap(), CI);
    let reply = said(&mut answered);
    assert!(!reply.is_error);
    assert!(reply.text.starts_with("The user saved it for this repository: .argonauts/config.toml, .argonauts/roles/reviewer.md"), "{}", reply.text);
    let closed = rig.events().into_iter().any(|event| {
        matches!(event, AppEvent::ChoiceClosed { outcome, .. } if outcome == "Save for this repository")
    });
    assert!(closed);
}

#[test]
fn saving_for_all_repositories_writes_in_your_home_folder() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(the_setup(), SAID);
    let question = rig.question();
    let effects = rig.pick(&question, "all_repositories");
    assert_eq!(saved(&effects)[0].1, SettingScope::Global);
    let home = rig.test.env.paths.home().to_path_buf();
    let reviewers = std::fs::read_to_string(home.join("roles/reviewer.md")).unwrap();
    assert!(reviewers.contains("Treat a missing test"), "{reviewers}");
    assert!(!rig.test.repo.root().join(".argonauts/roles").exists());
    assert!(said(&mut answered).text.contains("all their repositories"));
}

#[test]
fn this_conversation_keeps_it_as_session_settings() {
    let mut rig = Rig::new();
    let mut setup = the_setup();
    setup["prompts"] = json!([{"prompt": "review/review", "text": "Review it."}]);
    let (_, mut answered) = rig.call(setup, SAID);
    let question = rig.question();
    assert_eq!(
        question.options[0].detail.as_deref(),
        Some("prompts and mods are kept only by a save")
    );
    let effects = rig.pick(&question, "conversation");
    let [(pairs, scope)] = saved(&effects).try_into().expect("one save");
    assert_eq!(scope, SettingScope::Session);
    assert!(pairs.contains(&(
        "roles.reviewer.instructions".to_owned(),
        "\"Treat a missing test as a required change.\"".to_owned()
    )));
    assert!(pairs.contains(&("ui.theme".to_owned(), "\"light\"".to_owned())));
    assert!(!rig.test.repo.root().join(".argonauts").exists());
    let reply = said(&mut answered).text;
    assert!(reply.contains("this conversation"), "{reply}");
    assert!(reply.contains("weren't written"), "{reply}");
}

/// On autopilot the setup's question is never asked: it holds for this conversation, nothing
/// is written, and one line says so. With work in the same message it holds for that request.
#[test]
fn on_autopilot_a_setup_holds_without_a_question() {
    use argonauts_core::settings::approvals::ApprovalMode;

    use crate::controller::conversation::direct::set_approval;

    let mut rig = Rig::new();
    set_approval(&mut rig.conversation, ApprovalMode::Autopilot);
    let (effects, mut answered) = rig.call(the_setup(), SAID);
    let events = rig.events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "{events:#?}"
    );
    let line = events
        .iter()
        .find_map(|event| match event {
            AppEvent::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .expect("one line says what was decided");
    assert!(
        line.starts_with("Decided on autopilot · your setup holds for this conversation."),
        "{line}"
    );
    let [(_, scope)] = saved(&effects).try_into().expect("one save");
    assert_eq!(scope, SettingScope::Session);
    assert!(!rig.test.repo.root().join(".argonauts").exists());
    let reply = said(&mut answered).text;
    assert!(reply.starts_with("The user is on autopilot"), "{reply}");
    assert!(reply.contains("this conversation"), "{reply}");

    let mut with_work = the_setup();
    with_work["request_too"] = json!(true);
    let (effects, mut answered) = rig.call(with_work, SAID);
    assert!(saved(&effects).is_empty(), "nothing is saved for a request");
    let events = rig.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppEvent::Notice { text, .. }
            if text.starts_with("Decided on autopilot · your setup holds for this request."))),
        "{events:#?}"
    );
    assert!(said(&mut answered).text.contains("Start that workflow now"));
}

/// Nobody is there on autopilot to confirm a safety setting, so a setup keeps only what needs
/// no confirming: a key that loosens one is left out and named with how to set it, one that only
/// tightens stays, and what is saved asks no "Change it?" after.
#[test]
fn on_autopilot_a_setup_leaves_out_what_only_you_confirm() {
    use argonauts_core::settings::approvals::ApprovalMode;

    use crate::controller::conversation::direct::set_approval;

    let risky = || {
        let mut setup = the_setup();
        setup["roles"] = json!([]);
        setup["settings"] = json!({
            "ui.theme": "light",
            "sandbox.network": false,
            "sandbox.mode": "none",
        });
        setup
    };
    let mut rig = Rig::new();
    set_approval(&mut rig.conversation, ApprovalMode::Autopilot);
    let (effects, mut answered) = rig.call(risky(), SAID);
    let [Effect::SaveConfirmedSettings { pairs, scope }] = effects.as_slice() else {
        panic!("one save that asks nothing more: {effects:?}");
    };
    assert_eq!(*scope, SettingScope::Session);
    let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, ["ui.theme", "sandbox.network"]);
    let events = rig.events();
    let line = events
        .iter()
        .find_map(|event| match event {
            AppEvent::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .expect("one line says what was decided");
    assert!(
        line.starts_with(
            "Decided on autopilot · your setup holds for this conversation, except \
             sandbox.mode: a safety setting is yours to confirm, and /config sets it."
        ),
        "{line}"
    );
    assert!(!line.contains("none"), "only what holds is listed: {line}");
    let reply = said(&mut answered).text;
    assert!(reply.contains("sandbox.mode wasn't changed"), "{reply}");

    // For a request, the workflow it starts takes what is left, so nothing refuses its start.
    let mut with_work = risky();
    with_work["request_too"] = json!(true);
    let (effects, _answered) = rig.call(with_work, SAID);
    assert!(effects.is_empty(), "{effects:?}");
    let held = super::request_settings(&mut rig.conversation, &TurnId::numbered(1));
    assert_eq!(held, ["ui.theme=\"light\"", "sandbox.network=false"]);

    // With nothing left to hold, the line says so and names the key.
    let mut only_safety = risky();
    only_safety["settings"] = json!({"sandbox.mode": "none"});
    rig.events();
    let (effects, mut answered) = rig.call(only_safety, SAID);
    assert!(effects.is_empty(), "{effects:?}");
    let events = rig.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppEvent::Notice { text, .. }
            if text == "Decided on autopilot · nothing changed: sandbox.mode is a safety \
                        setting, yours to confirm, and /config sets it.")),
        "{events:#?}"
    );
    assert!(said(&mut answered).text.contains("wasn't changed"));
}

/// A setup's question open when autopilot comes on closes with where the setup holds, as one
/// proposed on autopilot is kept, and the call that waited on your pick is answered.
#[test]
fn autopilot_coming_on_keeps_a_setup_whose_question_was_open() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(the_setup(), SAID);
    let question = rig.question();
    assert!(answered.try_recv().is_err(), "the call waits on your pick");

    let effects = super::settle_on_autopilot(&mut rig.test.ctx, &mut rig.conversation);
    let [(_, scope)] = saved(&effects).try_into().expect("one save");
    assert_eq!(scope, SettingScope::Session);
    let events = rig.events();
    assert!(
        matches!(events.as_slice(), [AppEvent::ChoiceClosed { id, outcome }]
            if id == &question.id
                && outcome == "Decided on autopilot · your setup holds for this conversation. \
                               Ask to save it for this repository or all of them."),
        "{events:#?}"
    );
    let reply = said(&mut answered).text;
    assert!(reply.starts_with("The user is on autopilot"), "{reply}");
    assert!(!super::owns(&rig.conversation, &question.id));
}

#[test]
fn this_request_holds_for_the_workflow_it_starts_and_no_other() {
    let mut rig = Rig::new();
    let mut setup = the_setup();
    setup["request_too"] = json!(true);
    let (_, mut answered) = rig.call(setup, SAID);
    let question = rig.question();
    assert_eq!(question.options[0].label, "This request");
    assert!(question.options[0].recommended);
    let effects = rig.pick(&question, "request");
    assert!(saved(&effects).is_empty(), "no setting changes");
    assert!(said(&mut answered).text.contains("Start that workflow now"));
    assert!(super::request_settings(&mut rig.conversation, &TurnId::numbered(2)).is_empty());
    let (_, _answered) = rig.call(the_setup(), SAID);
    let question = rig.question();
    rig.pick(&question, "request");
    let held = super::request_settings(&mut rig.conversation, &TurnId::numbered(1));
    assert!(held.contains(&"ui.theme=\"light\"".to_owned()), "{held:?}");
    assert!(
        held.iter()
            .any(|setting| setting.starts_with("roles.reviewer.instructions=")),
        "{held:?}"
    );
    assert!(
        super::request_settings(&mut rig.conversation, &TurnId::numbered(1)).is_empty(),
        "one workflow takes them"
    );
}

#[test]
fn not_now_and_esc_write_nothing() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(the_setup(), SAID);
    let question = rig.question();
    assert!(rig.pick(&question, "not_now").is_empty());
    assert!(said(&mut answered).text.contains("nothing changed"));
    let (_, mut answered) = rig.call(the_setup(), SAID);
    let question = rig.question();
    assert!(
        super::set_aside(
            &mut rig.test.ctx,
            &mut rig.conversation,
            question.id.clone()
        )
        .is_empty()
    );
    assert!(said(&mut answered).text.contains("nothing changed"));
    assert!(!super::owns(&rig.conversation, &question.id));
    assert!(!rig.test.repo.root().join(".argonauts").exists());
}

#[test]
fn a_wrong_value_goes_back_and_you_see_nothing() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(
        json!({"asked": "x", "settings": {"ui.theme": "plaid"}}),
        SAID,
    );
    let reply = said(&mut answered);
    assert!(reply.is_error);
    assert!(reply.text.contains("ui.theme"), "{}", reply.text);
    assert!(
        !rig.events()
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. }))
    );
}

#[test]
fn a_place_you_named_is_kept_without_a_question() {
    let mut rig = Rig::new();
    let mut setup = the_setup();
    setup["keep"] = json!("repository");
    setup["keep_says"] = json!("save that for this repository");
    let (effects, mut answered) = rig.call(
        setup,
        "reviewers are strict about tests, save that for this repository",
    );
    assert_eq!(saved(&effects)[0].1, SettingScope::Project);
    let events = rig.events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "no question"
    );
    assert!(said(&mut answered).text.contains("this repository"));
    let reviewers = rig.test.repo.root().join(".argonauts/roles/reviewer.md");
    assert!(reviewers.exists());
}

fn a_required_step() -> Value {
    json!({
        "asked": "Every workflow must run the benchmarks before sign-off.",
        "steps": [{
            "name": "Benchmark",
            "role": "worker",
            "does": "Run the benchmarks and report any slowdown over 5%.",
            "when": "before sign-off"
        }]
    })
}

#[test]
fn a_required_step_is_a_row_and_a_save_writes_always_toml() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(a_required_step(), SAID);
    let question = rig.question();
    assert_eq!(
        question.body[0],
        "workflow      \tadds to\t workflows/always.toml   \"Benchmark\" before sign-off, by the \
         workers"
    );
    assert_eq!(
        question.options[1].detail.as_deref(),
        Some("writes 1 file in .argonauts/ · d shows the diff")
    );
    let effects = rig.pick(&question, "repository");
    assert!(saved(&effects).is_empty(), "no setting changes");
    let path = rig
        .test
        .repo
        .root()
        .join(".argonauts/workflows/always.toml");
    let text = std::fs::read_to_string(&path).expect("written");
    let steps = argonauts_core::required::parse(&text).expect("valid");
    assert_eq!(steps[0].name, "Benchmark");
    assert_eq!(steps[0].when.as_deref(), Some("before sign-off"));
    assert!(said(&mut answered).text.contains("always.toml"));
}

#[test]
fn a_required_step_kept_for_the_conversation_holds_for_every_workflow() {
    let mut rig = Rig::new();
    let (_, _answered) = rig.call(a_required_step(), SAID);
    let question = rig.question();
    let shown = question.body.join("\n");
    assert!(
        shown.contains("every workflow has \"Benchmark\" before sign-off"),
        "{shown}"
    );
    rig.pick(&question, "conversation");
    assert!(!rig.test.repo.root().join(".argonauts").exists());
    for turn in [2, 3] {
        let steps = super::required_steps(&mut rig.conversation, &TurnId::numbered(turn));
        assert_eq!(steps.len(), 1, "turn {turn}");
        assert_eq!(steps[0].name, "Benchmark");
    }
}

#[test]
fn a_required_step_kept_for_a_request_holds_for_that_one() {
    let mut rig = Rig::new();
    let mut setup = a_required_step();
    setup["request_too"] = json!(true);
    let (_, _answered) = rig.call(setup, SAID);
    let question = rig.question();
    rig.pick(&question, "request");
    let steps = super::required_steps(&mut rig.conversation, &TurnId::numbered(1));
    assert_eq!(steps.len(), 1);
    assert!(super::required_steps(&mut rig.conversation, &TurnId::numbered(1)).is_empty());
}

#[test]
fn a_wrong_step_goes_back_and_you_see_nothing() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(
        json!({
            "asked": "x",
            "steps": [{"name": "Scan", "role": "tester", "when": "sometime"}]
        }),
        SAID,
    );
    let reply = said(&mut answered);
    assert!(reply.is_error);
    assert!(
        reply.text.contains("steps: \"Scan\": role"),
        "{}",
        reply.text
    );
    assert!(reply.text.contains("before <step>"), "{}", reply.text);
    assert!(
        !rig.events()
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. }))
    );
}

/// Picking where a setup applies is the confirmation: a limit saved from the question asks no
/// second "change a safety setting?" question. A place only your words named still asks.
#[test]
fn the_questions_pick_confirms_a_limit_and_a_named_place_still_asks() {
    let with_limit = || {
        let mut setup = the_setup();
        setup["settings"] = json!({"limits.spend_per_day": 5.0});
        setup
    };
    let mut rig = Rig::new();
    let (_, _answered) = rig.call(with_limit(), SAID);
    let question = rig.question();
    let effects = rig.pick(&question, "conversation");
    assert!(
        matches!(effects.as_slice(), [Effect::SaveConfirmedSettings { pairs, .. }]
            if pairs[0].0 == "limits.spend_per_day"),
        "{effects:?}"
    );

    let mut rig = Rig::new();
    let mut setup = with_limit();
    setup["keep"] = json!("conversation");
    setup["keep_says"] = json!("for this conversation");
    let (effects, _answered) = rig.call(setup, "never more than $5 a day, for this conversation");
    assert!(
        matches!(effects.as_slice(), [Effect::SaveSettings { .. }]),
        "{effects:?}"
    );
}

#[test]
fn a_confirmed_limit_is_written_with_no_safety_question() {
    let mut rig = Rig::new();
    let ctx = &mut rig.test.ctx;
    let mut settings = crate::controller::settings::SettingsState::new(ctx);
    crate::controller::settings::save_confirmed(
        ctx,
        &mut settings,
        vec![("limits.spend_per_day".to_owned(), "5.0".to_owned())],
        SettingScope::Global,
    )
    .unwrap();
    let global = std::fs::read_to_string(rig.test.repo.global_config()).unwrap();
    assert!(global.contains("spend_per_day = 5.0"), "{global}");
    assert!(
        !rig.events().iter().any(|event| matches!(event,
            AppEvent::Choice { choice } if choice.kind == ChoiceKind::ConfirmSafetySetting)),
        "no second question"
    );
}
