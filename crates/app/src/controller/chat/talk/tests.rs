//! The setup tool from the form to the files: the card, each place, Not now, and a place your
//! own words named.

use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind};
use argonauts_core::asking::AskReply;
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::names::TurnId;
use argonauts_core::settings::SettingScope;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::{Call, ChatState, setup};
use crate::controller::Effect;
use crate::controller::test_support::{HarnessOptions, TestCtx};

const SAID: &str = "reviewers should treat a missing test as a blocker. never spend more than \
                    $5 a day.";

struct Rig {
    test: TestCtx,
    chat: ChatState,
}

impl Rig {
    fn new() -> Self {
        Self::with(HarnessOptions::default())
    }

    /// With `config` as your own settings file.
    fn with_config(config: &'static str) -> Self {
        Self::with(HarnessOptions {
            prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
                std::fs::write(repo.global_config(), config).unwrap();
            })),
            ..HarnessOptions::default()
        })
    }

    fn with(options: HarnessOptions) -> Self {
        let (test, _) = TestCtx::new(options);
        let chat = ChatState::new(&test.ctx).expect("a conversation");
        Self { test, chat }
    }

    /// Calls the tool from turn 1, as the session would, and hands back the call's answer.
    fn call(&mut self, arguments: Value, said: &str) -> (Vec<Effect>, oneshot::Receiver<AskReply>) {
        let (reply, answered) = oneshot::channel();
        let call = Call {
            turn: TurnId::numbered(1),
            arguments,
            reply,
        };
        let effects = setup::propose(&mut self.test.ctx, &mut self.chat, call, said);
        (effects, answered)
    }

    fn events(&mut self) -> Vec<AppEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.test.events.try_recv() {
            events.push(event);
        }
        events
    }

    fn card(&mut self) -> AppChoice {
        self.events()
            .into_iter()
            .find_map(|event| match event {
                AppEvent::Choice { choice } if choice.kind == ChoiceKind::Setup => Some(choice),
                _ => None,
            })
            .expect("the setup's card")
    }

    fn pick(&mut self, card: &AppChoice, value: &str) -> Vec<Effect> {
        let action = AnswerAction::Choose {
            value: value.to_owned(),
        };
        super::answer(&mut self.test.ctx, &mut self.chat, card.id.clone(), action)
            .expect("an answer")
    }
}

fn the_setup() -> Value {
    json!({
        "asked": "Reviewers treat a missing test as a blocker; never more than $5 a day.",
        "settings": {"agents.codex.effort": "high", "ui.theme": "light"},
        "roles": [{"role": "reviewers", "text": "Treat a missing test as a required change."}]
    })
}

fn said(answered: &mut oneshot::Receiver<AskReply>) -> AskReply {
    answered.try_recv().expect("the call was answered")
}

fn saved(effects: &[Effect]) -> Vec<(Vec<(String, String)>, SettingScope)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SaveSettings { pairs, scope } => Some((pairs.clone(), *scope)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_valid_setup_is_one_card_and_nothing_is_written_yet() {
    let mut rig = Rig::new();
    let (effects, mut answered) = rig.call(the_setup(), SAID);
    assert!(effects.is_empty());
    assert!(answered.try_recv().is_err(), "the call waits on your pick");
    let card = rig.card();
    assert_eq!(card.title, "Your setup");
    let table: Vec<&str> = card
        .body
        .iter()
        .take_while(|row| !row.is_empty())
        .map(String::as_str)
        .collect();
    assert_eq!(
        table,
        [
            "reviewers     adds to roles/reviewers.md   \"Treat a missing test as a required change.\"",
            "agents        codex.effort = \"high\"",
            "ui            theme = \"light\"",
        ]
    );
    let labels: Vec<&str> = card.options.iter().map(|o| o.label.as_str()).collect();
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
    assert!(card.options[0].recommended);
    assert_eq!(
        card.options[1].detail.as_deref(),
        Some("writes 2 files in .argonauts/")
    );
    assert_eq!(card.options[4].action, AnswerAction::ShowDiff);
    let shown = card.body.join("\n");
    assert!(shown.contains("You asked: Reviewers treat"), "{shown}");
    assert!(shown.contains(".argonauts/config.toml"), "{shown}");
    assert!(shown.contains(".argonauts/roles/reviewers.md"), "{shown}");
    assert!(!rig.test.env.project_config().exists());
}

#[test]
fn the_crew_reads_as_the_screen_names_agents_in_the_crews_order() {
    let mut rig = Rig::new();
    let setup = json!({
        "asked": "Fable plans, codex builds, and the critic is picked from the crew.",
        "settings": {
            "roles.worker.workers": ["codex/gpt-5.6"],
            "roles.critic.workers": [],
            "roles.orchestrator.workers": ["claude-code/claude-fable-5-1"],
            "ui.theme": "light"
        }
    });
    let (_, _answered) = rig.call(setup, SAID);
    let card = rig.card();
    let table: Vec<&str> = card
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
    let shown = card.body.join("\n");
    assert!(
        shown.contains("roles.orchestrator.workers = [\"claude-code/claude-fable-5-1\"]"),
        "what would be written keeps the ids: {shown}"
    );
}

/// A setup said in words: the crew as the screen names agents, how the
/// workers keep each task, the judge that follows the reviewers, the reviewers' file, then the
/// limit with what happens when it's reached.
#[test]
fn a_crew_said_in_words_reads_with_who_follows_whom_and_how_each_task_is_kept() {
    // Your crew names reviewers and no judge, so the judge follows them.
    let mut rig = Rig::with_config("[crew]\nreviewers = [\"codex\"]\n");
    let setup = json!({
        "asked": "I want fable to plan, codex to build using its own goal, and astra to review.",
        "settings": {
            "crew.orchestrator": ["claude-code/claude-fable-5-1"],
            "crew.workers": ["codex/gpt-5.6"],
            "roles.worker.goal": "cli",
            "crew.reviewers": ["codex/gpt-6-astra"],
            "limits.spend_per_day": 5.0
        },
        "roles": [{"role": "reviewers", "text": "Treat a missing test as a required change."}]
    });
    let (_, _answered) = rig.call(setup, SAID);
    let card = rig.card();
    let table: Vec<&str> = card
        .body
        .iter()
        .take_while(|row| !row.is_empty())
        .map(String::as_str)
        .collect();
    assert_eq!(
        table,
        [
            "crew          orchestrator   Fable 5.1 on Claude Code",
            "              workers        GPT 5.6 on Codex       each task as Codex's own goal",
            "              reviewers      GPT 6 Astra on Codex",
            "              judge          GPT 6 Astra on Codex   follows the reviewers",
            "reviewers     adds to roles/reviewers.md   \"Treat a missing test as a required change.\"",
            "limits        spend_per_day = 5.0                   when reached: fallback",
        ]
    );
}

#[test]
fn saving_for_the_repository_writes_its_files() {
    let mut rig = Rig::new();
    let roles = rig.test.repo.root().join(".argonauts/roles");
    std::fs::create_dir_all(&roles).unwrap();
    std::fs::write(
        roles.join("reviewers.md"),
        "---\nmode: add\n---\nKeep reviews short.\n",
    )
    .unwrap();
    let mut setup = the_setup();
    setup["prompts"] = json!([{"prompt": "review/review", "text": "Review it as I would."}]);
    // A mod is saved only once it reads as one: a body and the data it draws.
    const CI: &str = "title = \"CI\"\nbody = \"text\"\n[data]\nsource = \"spend\"\n";
    setup["mods"] = json!([{"name": "ci", "toml": CI}]);
    let (_, mut answered) = rig.call(setup, SAID);
    let card = rig.card();
    assert_eq!(
        card.options[1].detail.as_deref(),
        Some("writes 4 files in .argonauts/")
    );
    let effects = rig.pick(&card, "repository");
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
    let reviewers = std::fs::read_to_string(roles.join("reviewers.md")).unwrap();
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
    assert!(reply.text.starts_with("The user saved it for this repository: .argonauts/config.toml, .argonauts/roles/reviewers.md"), "{}", reply.text);
    let closed = rig.events().into_iter().any(|event| {
        matches!(event, AppEvent::ChoiceClosed { outcome, .. } if outcome == "Save for this repository")
    });
    assert!(closed);
}

#[test]
fn saving_for_all_repositories_writes_in_your_home_folder() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(the_setup(), SAID);
    let card = rig.card();
    let effects = rig.pick(&card, "all_repositories");
    assert_eq!(saved(&effects)[0].1, SettingScope::Global);
    let home = rig.test.env.paths.home().to_path_buf();
    let reviewers = std::fs::read_to_string(home.join("roles/reviewers.md")).unwrap();
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
    let card = rig.card();
    assert_eq!(
        card.options[0].detail.as_deref(),
        Some("prompts and mods are kept only by a save")
    );
    let effects = rig.pick(&card, "conversation");
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

#[test]
fn this_request_holds_for_the_workflow_it_starts_and_no_other() {
    let mut rig = Rig::new();
    let mut setup = the_setup();
    setup["request_too"] = json!(true);
    let (_, mut answered) = rig.call(setup, SAID);
    let card = rig.card();
    assert_eq!(card.options[0].label, "This request");
    assert!(card.options[0].recommended);
    let effects = rig.pick(&card, "request");
    assert!(saved(&effects).is_empty(), "no setting changes");
    assert!(said(&mut answered).text.contains("Start that workflow now"));
    assert!(super::request_settings(&mut rig.chat, &TurnId::numbered(2)).is_empty());
    let (_, _answered) = rig.call(the_setup(), SAID);
    let card = rig.card();
    rig.pick(&card, "request");
    let held = super::request_settings(&mut rig.chat, &TurnId::numbered(1));
    assert!(held.contains(&"ui.theme=\"light\"".to_owned()), "{held:?}");
    assert!(
        held.iter()
            .any(|setting| setting.starts_with("roles.reviewer.instructions=")),
        "{held:?}"
    );
    assert!(
        super::request_settings(&mut rig.chat, &TurnId::numbered(1)).is_empty(),
        "one workflow takes them"
    );
}

#[test]
fn not_now_and_esc_write_nothing() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(the_setup(), SAID);
    let card = rig.card();
    assert!(rig.pick(&card, "not_now").is_empty());
    assert!(said(&mut answered).text.contains("nothing changed"));
    let (_, mut answered) = rig.call(the_setup(), SAID);
    let card = rig.card();
    assert!(super::set_aside(&mut rig.test.ctx, &mut rig.chat, card.id.clone()).is_empty());
    assert!(said(&mut answered).text.contains("nothing changed"));
    assert!(!super::owns(&rig.chat, &card.id));
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
fn a_place_you_named_is_kept_without_a_card() {
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
        "no card"
    );
    assert!(said(&mut answered).text.contains("this repository"));
    let reviewers = rig.test.repo.root().join(".argonauts/roles/reviewers.md");
    assert!(reviewers.exists());
}

fn a_required_step() -> Value {
    json!({
        "asked": "Every workflow must run the benchmarks before sign-off.",
        "steps": [{
            "name": "Benchmark",
            "role": "workers",
            "does": "Run the benchmarks and report any slowdown over 5%.",
            "when": "before sign-off"
        }]
    })
}

#[test]
fn a_required_step_is_a_row_and_a_save_writes_always_toml() {
    let mut rig = Rig::new();
    let (_, mut answered) = rig.call(a_required_step(), SAID);
    let card = rig.card();
    assert_eq!(
        card.body[0],
        "workflow      adds to workflows/always.toml   \"Benchmark\" before sign-off, by the \
         workers"
    );
    assert_eq!(
        card.options[1].detail.as_deref(),
        Some("writes 1 file in .argonauts/")
    );
    let effects = rig.pick(&card, "repository");
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
    let card = rig.card();
    let shown = card.body.join("\n");
    assert!(
        shown.contains("every workflow has \"Benchmark\" before sign-off"),
        "{shown}"
    );
    rig.pick(&card, "conversation");
    assert!(!rig.test.repo.root().join(".argonauts").exists());
    for turn in [2, 3] {
        let steps = super::required_steps(&mut rig.chat, &TurnId::numbered(turn));
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
    let card = rig.card();
    rig.pick(&card, "request");
    let steps = super::required_steps(&mut rig.chat, &TurnId::numbered(1));
    assert_eq!(steps.len(), 1);
    assert!(super::required_steps(&mut rig.chat, &TurnId::numbered(1)).is_empty());
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
