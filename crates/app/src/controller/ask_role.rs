//! The main session asks a role of the crew: when you ask for it, the reviewers or the judge
//! read the change in your folder on their
//! own model, and the answer goes back to the main session as the tool's result. Nothing
//! merges from here, so it is their opinion, never a gate. What they spend goes to the usage
//! ledger with the conversation's turns.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::asking::AskReply;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::names::{SessionKey, WorkerRef};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::{StepError, StepRequest, StepRunner};
use argonauts_core::worker::{Access, SessionLimits, SessionSpec, Worker};
use argonauts_crew::claude_codes::{asked_of, review_of, said, verdict_schema};
use argonauts_crew::roles::candidates;
use argonauts_crew::spec::{SpecInput, build};
use time::OffsetDateTime;
use tokio::sync::oneshot;

use crate::controller::Ctx;
use crate::controller::turn::{part, vendor_out};
use crate::controller::workspace::working_tree_diff;
use crate::usage_ledger::UsageLedger;

/// The roles the main session may ask: the reviewers, when someone besides the main agent
/// reviews, and the judge, when it has seats of its own.
pub(crate) fn roles(settings: &Settings) -> Vec<CrewRole> {
    let seats = |role: CrewRole| candidates(settings, role.fine_role());
    let main = [settings.main_agent.clone()];
    let reviewers = seats(CrewRole::Reviewers);
    let judge = seats(CrewRole::Judge);
    let mut roles = Vec::new();
    if reviewers != main {
        roles.push(CrewRole::Reviewers);
    }
    if judge != reviewers && judge != main {
        roles.push(CrewRole::Judge);
    }
    roles
}

/// What the main session asked, and of whom.
pub(crate) struct Asked {
    pub role: CrewRole,
    pub what: String,
    /// Your message, which the change is meant to do.
    pub request: String,
    /// Where the main session works: your checkout, or the folder it carries on in.
    pub folder: PathBuf,
}

/// Asks the role's first seat that can work now, on its own task, and answers through `reply`
/// once it has read the change.
pub(crate) fn answer(ctx: &Ctx, asked: Asked, reply: oneshot::Sender<AskReply>) {
    let settings = ctx.settings();
    let seat = candidates(settings, asked.role.fine_role())
        .into_iter()
        .filter(|seat| !vendor_out(ctx, seat))
        .find_map(|seat| Some((ctx.services.workers.get(&seat).ok()?, seat)));
    let Some((handle, seat)) = seat else {
        let _ = reply.send(AskReply {
            text: format!(
                "None of the crew's {} can work now, so nobody was asked.",
                asked.role.as_str()
            ),
            is_error: true,
        });
        return;
    };
    let settings = settings.clone();
    let steps = Arc::clone(&ctx.services.steps);
    let places = Places {
        sessions: ctx.env.chat_dir().join("asks"),
        state_root: ctx
            .env
            .state_dir
            .parent()
            .map_or_else(|| ctx.env.state_dir.clone(), Path::to_path_buf),
        ledger: UsageLedger::new(&ctx.env.paths),
        repo: ctx.env.repo_root.clone(),
    };
    tokio::spawn(async move {
        let text = match ask(&settings, steps, handle, &seat, &asked, &places).await {
            Ok(text) => AskReply::text(text),
            Err(err) => AskReply {
                text: format!("Argonauts couldn't ask them: {err:#}"),
                is_error: true,
            },
        };
        let _ = reply.send(text);
    });
}

/// Where an ask keeps its session, what the seat may not read, and where its spend goes.
struct Places {
    /// The asks' sessions, in the conversation's folder.
    sessions: PathBuf,
    /// Argonauts' own state.
    state_root: PathBuf,
    ledger: UsageLedger,
    /// The repository the conversation is in, which the ledger files the spend under.
    repo: PathBuf,
}

async fn ask(
    settings: &Settings,
    steps: Arc<dyn StepRunner>,
    handle: Arc<dyn Worker>,
    seat: &WorkerRef,
    asked: &Asked,
    places: &Places,
) -> anyhow::Result<String> {
    let folder = asked.folder.clone();
    let change = tokio::task::spawn_blocking(move || working_tree_diff(&folder)).await??;
    if change.trim().is_empty() {
        return Ok("There is no change in the folder yet, so nothing was reviewed.".to_owned());
    }
    let spec = spec(
        settings,
        handle.as_ref(),
        seat,
        asked,
        &change,
        &places.sessions,
        places.state_root.clone(),
    )?;
    let ran = steps
        .run(StepRequest::new(handle, spec), Box::new(|_| {}))
        .await;
    // A session that ended without a usable answer spent all the same.
    let end = match &ran {
        Ok(outcome) => Some(&outcome.end),
        Err(StepError::Session(end)) => Some(end.as_ref()),
        Err(_) => None,
    };
    if let Some(end) = end {
        let now = OffsetDateTime::now_utc();
        places
            .ledger
            .turn_ended(&places.repo, seat, &end.spend, now);
    }
    Ok(said(seat, &review_of(&ran?.value)))
}

/// A read-only session of the seat in the main session's folder, asked as a review or a
/// sign-off in Claude Code's own workflow is.
fn spec(
    settings: &Settings,
    handle: &dyn Worker,
    seat: &WorkerRef,
    asked: &Asked,
    change: &str,
    session_dir: &Path,
    state_root: PathBuf,
) -> anyhow::Result<SessionSpec> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let key = SessionKey::parse(&format!("ask-{}", &id[..12]))
        .ok_or_else(|| anyhow::anyhow!("no key for the ask"))?;
    let session_dir = session_dir.join(key.as_str());
    create_private_dir_all(&session_dir)?;
    let mut prompt = vec![
        part(
            InputPartKind::Prompt,
            asked.role.as_str(),
            asked_of(asked.role).to_owned(),
        ),
        part(InputPartKind::Goal, "the request", asked.request.clone()),
    ];
    if !asked.what.trim().is_empty() {
        prompt.push(part(
            InputPartKind::Message,
            "what it does",
            asked.what.clone(),
        ));
    }
    prompt.push(part(
        InputPartKind::Evidence,
        "the change",
        change.to_owned(),
    ));
    let mut spec = build(
        settings,
        SpecInput {
            run: argonauts_core::views::conversation_run(),
            key,
            role: asked.role.fine_role(),
            worker: seat.clone(),
            kind: handle.kind(),
            card: None,
            step: None,
            access: Access::ReadOnly,
            deny_read: vec![state_root],
            workdir: asked.folder.clone(),
            session_dir,
            prompt,
            json_schema: Some(verdict_schema()),
            resume: None,
            lifetime: SessionLifetime::Run,
            limits: SessionLimits::default(),
            goal_sha256: None,
            status_file: None,
            blind: false,
            own_workflow: false,
            autopilot: false,
            reviews: false,
        },
    )?;
    // It belongs to the conversation, not to a run.
    spec.run = None;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use argonauts_core::asking::{Ask, AskLine};
    use argonauts_core::settings::crew::{CrewSettings, Orchestrator};
    use argonauts_testkit::{FakeSession, FakeWorker};
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    use super::*;
    use crate::controller::test_support::{Harness, HarnessOptions, WAIT};

    /// The main session's change in your checkout: Codex reviews, and its answer comes back to
    /// the main session as the tool's result, read-only and on Codex's own model.
    #[tokio::test]
    async fn the_main_session_asks_the_reviewers_about_its_change() {
        let harness = Harness::start_with(HarnessOptions {
            prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
                repo.write("money.py", "sign = 1\n").unwrap();
                repo.commit_all("money").unwrap();
                repo.write("money.py", "sign = -1\n").unwrap();
                std::fs::write(
                    repo.global_config(),
                    "[solo_or_crew]\nforce = \"solo\"\n[crew]\nreviewers = [\"codex\"]\n",
                )
                .unwrap();
            })),
            ..HarnessOptions::default()
        })
        .await;
        let codex = harness.directory.add(FakeWorker::harness("codex", "gpt"));
        codex.push(
            FakeSession::structured(json!({
                "accept": false,
                "summary": "The sign flips every amount",
                "findings": ["money.py: sign must stay 1"],
            }))
            .with_spend(argonauts_core::Spend {
                input_tokens: 1200,
                output_tokens: 80,
                cost_usd: 0.004,
                ..argonauts_core::Spend::default()
            }),
        );
        harness.agent.push(
            FakeSession::completed("Asked the reviewers.").with_delay(Duration::from_secs(3)),
        );
        harness
            .submit("Fix the sign, then have the reviewers look")
            .await;
        let deadline = tokio::time::Instant::now() + WAIT;
        let spec = loop {
            if let Some(spec) = harness.agent.specs().into_iter().next() {
                break spec;
            }
            assert!(tokio::time::Instant::now() < deadline, "no session started");
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let asking = spec.asking.expect("the main session listens for asks");
        // The judge nobody set follows the reviewers, so there's one role to ask.
        assert_eq!(asking.roles, [CrewRole::Reviewers]);

        let mut stream = TcpStream::connect(&asking.endpoint).await.unwrap();
        let line = AskLine {
            token: asking.token,
            ask: Ask::Role {
                role: CrewRole::Reviewers,
                label: String::new(),
                what: "Flip the sign".to_owned(),
            },
        };
        let text = format!("{}\n", serde_json::to_string(&line).unwrap());
        stream.write_all(text.as_bytes()).await.unwrap();
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).await.unwrap();
        let reply: AskReply = serde_json::from_str(answer.trim()).unwrap();
        assert!(!reply.is_error, "{reply:?}");
        assert!(
            reply
                .text
                .contains("asks for changes: The sign flips every amount"),
            "{}",
            reply.text
        );
        assert!(reply.text.contains("- money.py: sign must stay 1"));
        let asked = codex.specs().remove(0);
        assert_eq!(asked.access, Access::ReadOnly);
        assert_eq!(asked.run, None);
        let change = asked
            .prompt
            .iter()
            .find(|part| part.label == "the change")
            .expect("the change is in the prompt");
        assert!(change.text.contains("-sign = 1"), "{}", change.text);
        // What the reviewer spent is counted with the conversation's turns.
        let ledger = UsageLedger::new(&argonauts_core::Paths::new(harness.repo.home()));
        let lines = ledger.read_since(None);
        let spent = lines.iter().find(|line| line.agent == "codex");
        assert!(spent.is_some_and(|line| line.run.is_none()), "{lines:?}");
        harness.exit().await;
    }

    fn worker(name: &str) -> WorkerRef {
        name.parse().unwrap()
    }

    /// The main session is offered the reviewers when someone besides itself reviews, and the
    /// judge only when it has seats of its own.
    #[test]
    fn the_main_session_may_ask_the_roles_someone_else_fills() {
        let mut settings = Settings {
            main_agent: worker("claude-code/fable-5-1"),
            ..Settings::default()
        };
        settings.crew = CrewSettings {
            orchestrator: Orchestrator::Main,
            workers: Vec::new(),
            reviewers: vec![worker("astra")],
            judge: Vec::new(),
            worker_workflows: false,
        };
        assert_eq!(roles(&settings), [CrewRole::Reviewers]);
        settings.crew.judge = vec![worker("codex")];
        assert_eq!(roles(&settings), [CrewRole::Reviewers, CrewRole::Judge]);
        settings.crew.reviewers = vec![settings.main_agent.clone()];
        settings.crew.judge = Vec::new();
        assert!(roles(&settings).is_empty(), "nobody but itself");
    }
}
