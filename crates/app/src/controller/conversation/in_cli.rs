//! How a session carries on in its CLI's own screen: the command the rows and lines print, `o`,
//! which opens it there with the screen out of the way, and the turns made there, read back
//! under one line when the conversation comes back to it.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use anyhow::Context as _;
use argonauts_conductor::cli_runner::request::take_over_request;
use argonauts_core::app::{AppEvent, ConversationSummary, LineFacts, ReplayTurn};
use argonauts_core::cli_sessions::{
    self, CliScan, CliSession, CliSessionRef, CliTurn, InItsCli, Speaker,
};
use argonauts_core::events::run::Party;
use argonauts_core::names::{ConversationId, ModelRef, SeatId, SessionKey};
use argonauts_core::{NoteLevel, RunId};
use argonauts_registry::ResolvedSettings;
use time::OffsetDateTime;

use super::{ConversationState, cli, past, settled};
use crate::FoundSession;
use crate::controller::{Ctx, Effect};
use crate::conversation_store::ConversationStore;
use crate::env::AppEnv;

/// How many of a session's last turns are read for the ones made in its CLI.
const READ_BACK: usize = 40;

/// The newest conversations in `/resume` whose turns made in their CLIs are counted. Each
/// count reads a transcript, so the older ones say only how to carry on.
const COUNTED: usize = 8;

/// A session as a conversation holds it: whose it is and where it carries on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    worker: ModelRef,
    cli_session: String,
    folder: PathBuf,
}

impl Held {
    fn reference(&self) -> CliSessionRef {
        CliSessionRef {
            agent_id: self.worker.agent_id(),
            id: self.cli_session.clone(),
        }
    }

    fn of(session: &CliSession) -> Self {
        Self {
            worker: ModelRef::new(session.agent_id.0.clone(), session.model.clone()),
            cli_session: session.id.clone(),
            folder: session.folder.clone(),
        }
    }
}

/// The main session conversation `id` keeps in its record. A record that only names a session
/// to branch from holds none of its own yet.
fn held_by(ctx: &Ctx, id: &ConversationId) -> Option<(Held, ConversationStore)> {
    let store = ConversationStore::open(&ctx.env.conversation_dir(), id);
    let session = store.session().filter(|session| !session.fork)?;
    let folder = store
        .place()
        .map_or_else(|| ctx.env.repo_root.clone(), |place| place.folder);
    let held = Held {
        worker: session.worker,
        cli_session: session.cli_session,
        folder,
    };
    Some((held, store))
}

/// The conversation on screen holds its session in memory.
fn held_here(ctx: &Ctx, conversation: &ConversationState) -> Option<Held> {
    let session = conversation
        .session
        .as_ref()
        .filter(|session| !session.fork)?;
    let folder = conversation
        .place
        .as_ref()
        .map_or_else(|| ctx.env.repo_root.clone(), |place| place.folder.clone());
    Some(Held {
        worker: session.worker.clone(),
        cli_session: session.cli_session.clone(),
        folder,
    })
}

/// When a conversation last did anything, which is when you left its session: its journal's
/// last write.
pub(super) fn left_off(store: &ConversationStore) -> Option<OffsetDateTime> {
    std::fs::metadata(store.journal_path())
        .and_then(|meta| meta.modified())
        .ok()
        .map(OffsetDateTime::from)
}

/// What the rows print for `held`, with no turns counted yet. A conversation's session has its
/// title, which a CLI may know it by.
fn described(
    ctx: &Ctx,
    held: &Held,
    conversation: Option<ConversationId>,
    title: Option<&str>,
) -> Option<InItsCli> {
    let adapter = cli::adapter(ctx, &held.worker.agent)?;
    let mut req = take_over_request(
        &held.worker,
        &held.cli_session,
        &held.folder,
        adapter.as_ref(),
        ctx.settings(),
        Some(ctx.services.registry.as_ref()),
    );
    req.preferences.title = title
        .filter(|title| !title.trim().is_empty())
        .map(str::to_owned);
    let command = adapter.resume_command(&req)?;
    Some(InItsCli {
        session: held.reference(),
        conversation,
        cli: adapter.display_name().to_owned(),
        command: InItsCli::typed(&command, &held.folder, &ctx.env.repo_root),
        local_model: command.local_model,
        also: command.also,
        made_there: 0,
    })
}

/// The turns made in `held`'s CLI after `left`, read from its transcript on a blocking thread.
/// A transcript that can't be read has none.
async fn made_since(ctx: &Ctx, held: &Held, left: OffsetDateTime) -> Vec<CliTurn> {
    let (Some(adapter), Some(home)) = (
        cli::adapter(ctx, &held.worker.agent),
        ctx.env.user_home.clone(),
    ) else {
        return Vec::new();
    };
    let scan = CliScan::new(held.folder.clone(), home);
    let id = held.cli_session.clone();
    let read =
        tokio::task::spawn_blocking(move || adapter.cli_session_turns(&scan, &id, READ_BACK)).await;
    match read {
        Ok(Ok(turns)) => cli_sessions::made_since(&turns, left).to_vec(),
        Ok(Err(err)) => {
            tracing::warn!("reading the turns made in {}: {err:#}", held.worker.agent);
            Vec::new()
        }
        Err(err) => {
            tracing::warn!("reading the turns made in {}: {err}", held.worker.agent);
            Vec::new()
        }
    }
}

/// The turns as the conversation reprints them, oldest first.
pub(super) fn replayed(turns: &[CliTurn], worker: &ModelRef) -> Vec<ReplayTurn> {
    turns
        .iter()
        .map(|turn| ReplayTurn {
            from: match turn.from {
                Speaker::User => Party::User,
                Speaker::Agent => Party::Agent {
                    session: SessionKey::numbered(0),
                    worker: worker.clone(),
                },
            },
            text: turn.text.clone(),
            steps: turn.steps.clone(),
        })
        .collect()
}

/// `1 turn`, `2 turns`.
fn turns_said(count: u32) -> String {
    match count {
        1 => "1 turn".to_owned(),
        count => format!("{count} turns"),
    }
}

/// How each listed conversation's main session and each CLI session carries on in its CLI's
/// own screen, for `/resume`. The newest conversations also count the turns made there since
/// they last used it.
pub(crate) async fn listed(
    ctx: &Ctx,
    conversations: &[ConversationSummary],
    sessions: &[CliSession],
) -> Vec<InItsCli> {
    let mut in_cli = Vec::new();
    for (n, summary) in conversations.iter().enumerate() {
        let Some((held, store)) = held_by(ctx, &summary.conversation) else {
            continue;
        };
        let conversation = Some(summary.conversation.clone());
        let Some(mut row) = described(ctx, &held, conversation, Some(&summary.title)) else {
            continue;
        };
        if n < COUNTED
            && let Some(left) = left_off(&store)
        {
            row.made_there = cli_sessions::prompts(&made_since(ctx, &held, left).await);
        }
        in_cli.push(row);
    }
    in_cli.extend(
        sessions
            .iter()
            .filter_map(|session| described(ctx, &Held::of(session), None, None)),
    );
    in_cli
}

/// What the conversation on screen was last told, kept while its conversation, session and
/// title stay the same, since the bottom line asks often.
static HERE: Mutex<Option<(Asked, Option<InItsCli>)>> = Mutex::new(None);

/// What [`here`] answers from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    conversation: ConversationId,
    held: Held,
    title: String,
}

/// How the conversation on screen carries on in its CLI's own screen, for the Status tab and
/// the lines left on quit. `None` before its CLI has a session.
pub(crate) fn here(ctx: &Ctx, conversation: &ConversationState) -> Option<InItsCli> {
    let asked = Asked {
        conversation: conversation.id.clone(),
        held: held_here(ctx, conversation)?,
        title: conversation.title.clone(),
    };
    let mut kept = HERE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((was, said)) = kept.as_ref()
        && *was == asked
    {
        return said.clone();
    }
    let said = described(
        ctx,
        &asked.held,
        Some(asked.conversation.clone()),
        Some(&asked.title),
    );
    *kept = Some((asked, said.clone()));
    said
}

/// The session the conversation on screen carries, by its CLI and id, which tells when the
/// lines that name it are due again.
pub(crate) fn carried(conversation: &ConversationState) -> Option<(String, String)> {
    let session = conversation
        .session
        .as_ref()
        .filter(|session| !session.fork)?;
    Some((session.worker.agent.clone(), session.cli_session.clone()))
}

/// What the lines left on quit say: how to resume the conversation on screen once it has a
/// turn, here or in its CLI's own screen.
pub(crate) fn for_the_line(ctx: &Ctx, conversation: &ConversationState, line: &mut LineFacts) {
    if conversation.turns == 0 {
        return;
    }
    line.conversation = Some(conversation.id.clone());
    line.in_cli = here(ctx, conversation);
}

/// A session opened in its CLI's own screen, until the screen comes back.
struct Opened {
    argv: Vec<String>,
    held: Held,
    cli: String,
    conversation: Option<ConversationId>,
    on_screen: bool,
    left: OffsetDateTime,
    /// A council seat carried on in its CLI, which goes back to the talk when it quits.
    seat: Option<(RunId, SeatId)>,
}

static OPENED: Mutex<Option<Opened>> = Mutex::new(None);

/// `o`: opens `session` in its CLI's own screen, here, with the screen out of the way. The
/// conversation comes back to it when that CLI quits: the one on screen draws the turns made
/// there, another conversation or a CLI's own session carries on as Enter would.
pub(crate) async fn open(
    ctx: &mut Ctx,
    conversation: &ConversationState,
    session: &CliSessionRef,
    carried_by: Option<&ConversationId>,
) -> anyhow::Result<Vec<Effect>> {
    open_as(ctx, conversation, session, carried_by, None).await
}

/// `o` twice on a council seat's session: it opens in its CLI's own screen as [`open`] opens
/// one, and when that CLI quits the seat goes back to its council's talk, as [`seat_back`]
/// says, rather than the conversation taking the session on.
pub(crate) async fn open_for_seat(
    ctx: &mut Ctx,
    conversation: &ConversationState,
    session: &CliSessionRef,
    seat: (RunId, SeatId),
) -> anyhow::Result<Vec<Effect>> {
    open_as(ctx, conversation, session, None, Some(seat)).await
}

/// A council seat the screen took the terminal back from.
pub(crate) struct SeatBack {
    pub run: RunId,
    pub seat: SeatId,
    held: Held,
    left: OffsetDateTime,
}

/// The council seat the screen took the terminal back from, if `argv` was one carried on in
/// its CLI.
pub(crate) fn seat_back(argv: &[String]) -> Option<SeatBack> {
    let mut opened = OPENED.lock().unwrap_or_else(PoisonError::into_inner);
    match opened.as_ref() {
        Some(open) if open.argv == argv && open.seat.is_some() => {
            let open = opened.take()?;
            let (run, seat) = open.seat?;
            Some(SeatBack {
                run,
                seat,
                held: open.held,
                left: open.left,
            })
        }
        _ => None,
    }
}

/// What you typed to the seat in its own CLI since it opened there, as the CLI's session
/// store keeps it.
pub(crate) async fn typed_to_seat(ctx: &Ctx, back: &SeatBack) -> Vec<String> {
    made_since(ctx, &back.held, back.left)
        .await
        .into_iter()
        .filter(|turn| turn.from == Speaker::User)
        .map(|turn| turn.text)
        .collect()
}

async fn open_as(
    ctx: &mut Ctx,
    conversation: &ConversationState,
    session: &CliSessionRef,
    carried_by: Option<&ConversationId>,
    seat: Option<(RunId, SeatId)>,
) -> anyhow::Result<Vec<Effect>> {
    let on_screen = carried_by == Some(&conversation.id);
    let (held, left) = match carried_by {
        Some(_) if on_screen => (held_here(ctx, conversation), left_off(&conversation.store)),
        Some(id) => match held_by(ctx, id) {
            Some((held, store)) => (Some(held), left_off(&store)),
            None => (None, None),
        },
        None => (Some(Held::of(&cli::locate(ctx, session).await?)), None),
    };
    let cli = cli::name(ctx, &session.agent_id.0);
    let Some(held) = held.filter(|held| held.reference() == *session) else {
        anyhow::bail!("{cli} has no session {session} to open. /resume lists them.");
    };
    if !settled(conversation) {
        ctx.notice(
            NoteLevel::Info,
            format!("A turn is still running. Esc stops it, then o opens it in {cli}."),
        );
        return Ok(Vec::new());
    }
    if !ctx.env.interactive {
        anyhow::bail!("o hands the terminal to {cli}, and there's no terminal here.");
    }
    let runner = ctx
        .services
        .workers
        .get(&held.worker)
        .or_else(|_| {
            ctx.services
                .workers
                .get(&ModelRef::new(held.worker.agent.clone(), None))
        })
        .with_context(|| format!("{cli} isn't set up here. /agents shows it."))?;
    // A seat goes on in its CLI as a run's session does, never as yours.
    let spec = if seat.is_some() {
        runner.seat_take_over_command(&held.cli_session, &held.folder)
    } else {
        runner.take_over_command(&held.cli_session, &held.folder)
    }
    .with_context(|| format!("{cli} can't open a session in its own screen."))?;
    let argv: Vec<String> = std::iter::once(spec.program).chain(spec.args).collect();
    *OPENED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Opened {
        argv: argv.clone(),
        held,
        cli,
        conversation: carried_by.cloned(),
        on_screen,
        left: left.unwrap_or_else(OffsetDateTime::now_utc),
        seat,
    });
    ctx.send(AppEvent::RunInTerminal {
        argv,
        cwd: spec.cwd,
        env_remove: spec.env_remove,
        env_set: spec.env_set,
        quiet: on_screen,
    });
    Ok(Vec::new())
}

/// The screen took the terminal back from `argv`. When it was a session `o` opened, the
/// conversation comes back to it.
pub(crate) async fn returned(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    argv: &[String],
) -> anyhow::Result<Vec<Effect>> {
    let opened = {
        let mut opened = OPENED.lock().unwrap_or_else(PoisonError::into_inner);
        match opened.as_ref() {
            Some(open) if open.argv == argv => opened.take(),
            _ => None,
        }
    };
    let Some(opened) = opened else {
        return Ok(Vec::new());
    };
    match &opened.conversation {
        Some(_) if opened.on_screen => {
            let turns = made_since(ctx, &opened.held, opened.left).await;
            let said = match cli_sessions::prompts(&turns) {
                0 => format!("Back from {}", opened.cli),
                made => format!("Back from {} · {} made there", opened.cli, turns_said(made)),
            };
            ctx.send(AppEvent::TurnsMadeInCli {
                said,
                turns: replayed(&turns, &opened.held.worker),
            });
            Ok(Vec::new())
        }
        Some(id) => super::resume(ctx, conversation, id).await,
        None => super::resume_cli_session(ctx, conversation, &opened.held.reference()).await,
    }
}

/// After `/resume` brought a conversation back: the turns made in its CLI's own screen since it
/// last used the session, drawn under one line. `left` is when it last did anything, from
/// before the resume.
pub(super) async fn since_you_left(
    ctx: &Ctx,
    conversation: &ConversationState,
    left: Option<OffsetDateTime>,
) {
    let (Some(held), Some(left)) = (held_here(ctx, conversation), left) else {
        return;
    };
    let turns = made_since(ctx, &held, left).await;
    let made = cli_sessions::prompts(&turns);
    if made == 0 {
        return;
    }
    let cli = cli::name(ctx, &held.worker.agent);
    ctx.send(AppEvent::TurnsMadeInCli {
        said: format!("{} made in {cli} since you left", turns_said(made)),
        turns: replayed(&turns, &held.worker),
    });
}

/// Where the session `id` is: the one conversation that carries it, else the one CLI that lists
/// it, from any folder, else the CLIs that keep no list and could carry it on.
pub(crate) fn find_any(
    env: &AppEnv,
    resolved: &ResolvedSettings,
    id: &str,
) -> anyhow::Result<FoundSession> {
    anyhow::ensure!(
        cli_sessions::is_session_id(id),
        "{id:?} isn't a session id, a conversation such as conversation-002, or a run such as \
         run-03"
    );
    let conversation_dir = env.conversation_dir();
    let carrying: Vec<ConversationId> = past::list(&conversation_dir, &[])
        .into_iter()
        .map(|summary| summary.conversation)
        .filter(|conversation| {
            ConversationStore::open(&conversation_dir, conversation)
                .session()
                .is_some_and(|session| !session.fork && session.cli_session.starts_with(id))
        })
        .collect();
    match carrying.as_slice() {
        [one] => return Ok(FoundSession::Conversation(one.clone())),
        [] => {}
        many => anyhow::bail!(
            "{id} could be the session of {} conversations. Type more of it, or pick one in \
             argonauts resume.",
            many.len()
        ),
    }
    let mut seen = std::collections::BTreeSet::new();
    let adapters: Vec<_> = crate::commands::adapters_here(Some(env), resolved)?
        .into_iter()
        // A CLI can be both built in and set up again as a CLI.
        .filter(|adapter| seen.insert(adapter.id()))
        .collect();
    let named = |adapter: &dyn argonauts_core::Adapter| CliSessionRef {
        agent_id: argonauts_core::names::AgentId(adapter.id().to_owned()),
        id: id.to_owned(),
    };
    let listing: Vec<CliSessionRef> = match env.user_home.clone() {
        Some(home) => {
            let scan = CliScan::new(env.repo_root.clone(), home).everywhere();
            adapters
                .iter()
                .filter(|adapter| adapter.lists_sessions())
                .filter_map(|adapter| {
                    cli::find(adapter.as_ref(), &scan, &named(adapter.as_ref())).ok()
                })
                .map(|session| session.reference())
                .collect()
        }
        None => Vec::new(),
    };
    match listing.as_slice() {
        [one] => return Ok(FoundSession::CliSession(one.clone())),
        [] => {}
        many => anyhow::bail!(
            "{id} could be a session of more than one CLI. Name the CLI, as in argonauts resume \
             {}.",
            many[0]
        ),
    }
    let reopening = argonauts_core::session::SessionRequest::reopening(id, &env.repo_root);
    Ok(FoundSession::Unlisted(
        adapters
            .iter()
            .filter(|adapter| !adapter.lists_sessions())
            .filter(|adapter| adapter.resume_command(&reopening).is_some())
            .map(|adapter| (adapter.display_name().to_owned(), named(adapter.as_ref())))
            .collect(),
    ))
}
