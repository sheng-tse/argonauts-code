//! `argonauts behaviors --summaries [--since <period>]`: an update of Summaries from a shell,
//! with the summarizer and embedder the settings give, asking at the terminal where the tab
//! would ask. Ctrl+C stops it between requests, keeping every summary it wrote.

use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use argonauts_core::deep_map::{
    DeepDone, DeepProgress, DeepScope, DeepStart, EmbedderNote, OfferPlace, ScopeOffer,
    SummarizerOffer,
};
use tokio_util::sync::CancellationToken;

use super::service::DeepMaps;
use crate::behaviors::BehaviorPeriod;

/// How the command talks with you.
pub trait DeepTerminal: Send {
    /// One line of its own.
    fn line(&mut self, text: &str);
    /// How far it is, written over the last such line where the output is a terminal.
    fn progress(&mut self, text: &str);
    /// Your yes or no to a question: its lines, the last one the question. `None` when nobody
    /// is at a terminal to answer.
    fn ask(&mut self, question: &[String]) -> Option<bool>;
}

/// Updates Summaries. `Ok(false)` when it stopped or the summarizer failed, with every
/// summary it wrote kept.
pub async fn behaviors_deep(
    since: Option<BehaviorPeriod>,
    overrides: Vec<String>,
    terminal: &mut dyn DeepTerminal,
) -> anyhow::Result<bool> {
    let scope = match since {
        Some(since) => DeepScope::Since { at: since.start() },
        None => DeepScope::Everything,
    };
    let maps = DeepMaps::here(overrides)?;
    if maps.scope(scope.clone(), true).actions == 0 {
        terminal.line(
            "No actions recorded there yet: the map is made of each run's and conversation's \
             tool calls.",
        );
        return Ok(true);
    }
    let offer = maps.offer().await;
    let auto = maps.settings().behaviors.summarizer_is_auto();
    let chosen = offer
        .summarizers
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("no model to summarize with: no server on this computer answered, and no CLI is installed"))?;
    let counts = maps.scope(scope.clone(), chosen.local());
    if !chosen.local() {
        let local = offer.summarizers.iter().find(|offer| offer.local());
        match terminal.ask(&hosted_question(&chosen, &counts, local)) {
            Some(true) => {}
            Some(false) => {
                terminal.line("Nothing was sent.");
                return Ok(true);
            }
            None => bail!(
                "a hosted summarizer reads every action, so it asks first: run this at a \
                 terminal, or set behaviors.summarizer to a model on this computer"
            ),
        }
    }
    let mut redo = false;
    if let Some((made_by, actions)) = maps.redo(&chosen.model) {
        let made_by = offer
            .made_by
            .as_ref()
            .filter(|kept| kept.model == made_by)
            .map_or_else(|| made_by.to_string(), |kept| kept.name.clone());
        let question = vec![
            format!(
                "Summaries were made by {made_by}. One map never mixes two summarizers, so \
                 its {actions} actions would be summarized again with {}; the summaries \
                 already written are kept.",
                chosen.name
            ),
            "Summarize them again?".to_owned(),
        ];
        if terminal.ask(&question) != Some(true) {
            terminal.line("The map stays as it was.");
            return Ok(true);
        }
        redo = true;
    }
    let download_embedder = match &offer.embedder {
        EmbedderNote::Download { name, megabytes } => {
            let question = vec![
                format!(
                    "The embedder, {name}, turns each summary into a vector, so summaries that \
                     mean the same land together. It's {megabytes} MB, downloaded once."
                ),
                "Download it? Without it the summaries are placed by their words.".to_owned(),
            ];
            terminal.ask(&question) == Some(true)
        }
        _ => false,
    };
    let already = maps.already(&scope, &chosen.model);
    terminal.line(&start_words(&counts, &chosen, auto, already));
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let ctrl_c = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            stopping.cancel();
        }
    });
    let started = Instant::now();
    let start = DeepStart {
        scope,
        summarizer: chosen.model.clone(),
        download_embedder,
        redo,
    };
    let mut progress = |progress: DeepProgress| {
        terminal.progress(&progress_words(
            progress.done,
            progress.total,
            progress.seconds_left,
        ));
    };
    let done = maps.update(&start, &stop, &mut progress, &mut || {}).await;
    ctrl_c.abort();
    let done = done.context("keeping Summaries")?;
    for line in done_words(&done, started.elapsed()) {
        terminal.line(&line);
    }
    Ok(!done.stopped && done.failed.is_none())
}

/// `Summarizing 562 actions with Qwen3 Coder 30B on Ollama (auto) · 0 already done`.
fn start_words(
    counts: &ScopeOffer,
    chosen: &SummarizerOffer,
    auto: bool,
    already: usize,
) -> String {
    format!(
        "Summarizing {} actions with {}{} · {already} already done",
        counts.actions,
        chosen.name,
        if auto { " (auto)" } else { "" }
    )
}

/// The question before a hosted model reads every action, the other CLIs' too.
fn hosted_question(
    chosen: &SummarizerOffer,
    counts: &ScopeOffer,
    local: Option<&SummarizerOffer>,
) -> Vec<String> {
    let (cli, plan, provider) = match &chosen.place {
        OfferPlace::Plan {
            cli,
            plan,
            provider,
        } => (cli.as_str(), plan.as_str(), provider.as_str()),
        OfferPlace::ThisMachine { .. } => ("", "", ""),
    };
    let others: Vec<String> = counts
        .by_cli
        .iter()
        .filter(|count| count.cli != cli && count.actions > 0)
        .map(|count| format!("{} {}", count.actions, count.name))
        .collect();
    let including = match others.as_slice() {
        [] => String::new(),
        [one] => format!(", including the {one} actions"),
        [rest @ .., last] => format!(", including the {} and {last} actions", rest.join(", ")),
    };
    let mut question = vec![
        format!(
            "Send {} actions to {provider} for summaries?",
            counts.actions
        ),
        format!(
            "{} would read each action as it ran: commands, file paths and the lines they \
             touched{including}. It uses about {}k tokens of your {plan} plan. Nothing is sent \
             anywhere else.",
            chosen.name,
            counts.tokens.div_ceil(1000)
        ),
    ];
    if let Some(local) = local {
        question.push(format!(
            "{} on this computer would do it for free: set behaviors.summarizer to {}.",
            local.short, local.model
        ));
    }
    question.push(format!("Summarize with {}?", chosen.short));
    question
}

/// `312 of 562 · about 50 s left`.
pub(crate) fn progress_words(done: usize, total: usize, seconds_left: Option<u64>) -> String {
    let left = seconds_left
        .map(|seconds| {
            format!(
                " · about {} left",
                duration_words(Duration::from_secs(seconds))
            )
        })
        .unwrap_or_default();
    format!("{done} of {total}{left}")
}

/// `50 s`, `2 min`, `1 min 50 s`.
fn duration_words(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds} s"),
        _ if seconds.is_multiple_of(60) => format!("{} min", seconds / 60),
        _ => format!("{} min {} s", seconds / 60, seconds % 60),
    }
}

/// What the update did, in a line or three.
pub(crate) fn done_words(done: &DeepDone, took: Duration) -> Vec<String> {
    let mut lines = Vec::new();
    let made = format!(
        "{} summarized now, {} like an identical action, {} already done",
        done.written, done.reused, done.already
    );
    if done.stopped {
        lines.push(format!(
            "Stopped after {}: {made}. Every summary written is kept, and the next update \
             carries on.",
            duration_words(took)
        ));
    } else if let Some(why) = &done.failed {
        lines.push(format!(
            "The summarizer stopped answering after {} ({why}): {made}. Every summary written is \
             kept.",
            duration_words(took)
        ));
    } else {
        lines.push(format!("Done in {}: {made}.", duration_words(took)));
    }
    match &done.embedder_failed {
        Some(why) if done.by_words => lines.push(format!(
            "The summaries are placed by their words, since {why}."
        )),
        Some(why) => lines.push(format!(
            "The embedder didn't answer ({why}), so no new dot was placed; the summaries wait \
             for the next update."
        )),
        None if done.by_words => {
            lines.push("No embedding model, so the summaries are placed by their words.".to_owned())
        }
        None => {}
    }
    lines.push(format!(
        "Summaries place {} dots, {} of them new. d in Overview › Behaviors shows them.",
        done.placed, done.new_places
    ));
    lines
}

#[cfg(test)]
mod tests {
    use argonauts_core::deep_map::CliCount;
    use argonauts_core::model_picker::Fit;

    use super::*;

    fn page_counts() -> ScopeOffer {
        ScopeOffer {
            scope: DeepScope::Everything,
            actions: 562,
            projects: 6,
            by_cli: [
                ("claude-code", "Claude Code", 428),
                ("codex", "Codex", 115),
                ("antigravity-cli", "agy", 19),
            ]
            .iter()
            .map(|(cli, name, actions)| CliCount {
                cli: (*cli).to_owned(),
                name: (*name).to_owned(),
                actions: *actions,
            })
            .collect(),
            seconds: 112,
            tokens: 55_000,
        }
    }

    #[test]
    fn the_first_line_matches_the_shell_in_its_picture() {
        let qwen = SummarizerOffer {
            model: "ollama/qwen3-coder:30b-64k".parse().unwrap(),
            name: "Qwen3 Coder 30B on Ollama".to_owned(),
            short: "Qwen3 Coder 30B".to_owned(),
            place: OfferPlace::ThisMachine {
                bytes: None,
                fit: Some(Fit::Fits),
            },
        };
        // config_rows' shell lines: the command, then this, then the progress.
        assert_eq!(
            start_words(&page_counts(), &qwen, true, 0),
            "Summarizing 562 actions with Qwen3 Coder 30B on Ollama (auto) · 0 already done"
        );
    }

    #[test]
    fn a_hosted_question_names_the_other_clis_and_the_free_model() {
        let haiku = SummarizerOffer {
            model: "claude-code/haiku".parse().unwrap(),
            name: "Haiku 5.5 on Claude Code".to_owned(),
            short: "Haiku 5.5".to_owned(),
            place: OfferPlace::Plan {
                cli: "claude-code".to_owned(),
                plan: "Claude".to_owned(),
                provider: "Anthropic".to_owned(),
            },
        };
        let question = hosted_question(&haiku, &page_counts(), None);
        assert_eq!(question[0], "Send 562 actions to Anthropic for summaries?");
        assert!(question[1].contains("including the 115 Codex and 19 agy actions"));
        assert!(question[1].contains("about 55k tokens of your Claude plan"));
        assert_eq!(question.last().unwrap(), "Summarize with Haiku 5.5?");
    }

    #[test]
    fn progress_reads_as_the_tab_says_it() {
        assert_eq!(
            progress_words(312, 562, Some(50)),
            "312 of 562 · about 50 s left"
        );
        assert_eq!(progress_words(0, 562, None), "0 of 562");
        assert_eq!(duration_words(Duration::from_secs(110)), "1 min 50 s");
    }

    #[test]
    fn the_last_lines_say_what_was_kept() {
        let done = DeepDone {
            written: 480,
            reused: 82,
            placed: 562,
            new_places: 562,
            ..DeepDone::default()
        };
        assert_eq!(
            done_words(&done, Duration::from_secs(110)),
            [
                "Done in 1 min 50 s: 480 summarized now, 82 like an identical action, 0 already done.",
                "Summaries place 562 dots, 562 of them new. d in Overview › Behaviors shows them.",
            ]
        );
        let stopped = DeepDone {
            stopped: true,
            by_words: true,
            ..done
        };
        let lines = done_words(&stopped, Duration::from_secs(30));
        assert!(lines[0].starts_with("Stopped after 30 s"));
        assert_eq!(
            lines[1],
            "No embedding model, so the summaries are placed by their words."
        );
        let without = DeepDone {
            by_words: true,
            embedder_failed: Some("nomic-embed-text isn't on Ollama".to_owned()),
            ..DeepDone::default()
        };
        assert_eq!(
            done_words(&without, Duration::from_secs(30))[1],
            "The summaries are placed by their words, since nomic-embed-text isn't on Ollama."
        );
        let waiting = DeepDone {
            embedder_failed: Some("Ollama isn't running".to_owned()),
            ..DeepDone::default()
        };
        assert_eq!(
            done_words(&waiting, Duration::from_secs(30))[1],
            "The embedder didn't answer (Ollama isn't running), so no new dot was placed; the \
             summaries wait for the next update."
        );
    }
}
