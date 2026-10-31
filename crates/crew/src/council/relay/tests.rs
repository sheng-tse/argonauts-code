use argonauts_core::agents::adapter::{CitationForm, IdentityMarks};
use argonauts_core::base::names::{MessageId, ModelRef};
use argonauts_core::runs::events::council::{
    CouncilMessage, MessageKind, Point, PointState, ReplyTo, SeatRole, Seating, Speaker, Stance,
    TextPart,
};
use argonauts_core::settings::council::CouncilSeat;

use super::*;
use crate::council::letters::{self, Draw};

const OPUS: &str = "claude-code/claude-opus-5-5";
const LUNA: &str = "codex/gpt-5.6-luna";

const CLAUDE_MARKS: IdentityMarks = IdentityMarks {
    tool_names: &["Bash", "Read", "Edit", "Write", "TodoWrite", "Task"],
    row_marks: &["⏺", "⎿"],
    home_folders: &["~/.claude"],
    citation: None,
};

const CODEX_MARKS: IdentityMarks = IdentityMarks {
    tool_names: &["shell", "apply_patch", "update_plan"],
    row_marks: &["• Ran", "└"],
    home_folders: &["~/.codex"],
    citation: Some(CitationForm {
        open: "【F:",
        lines: "†L",
        close: "】",
    }),
};

fn provider(model: &ModelRef) -> String {
    match model.agent.as_str() {
        "claude-code" => "anthropic".to_owned(),
        "codex" => "openai".to_owned(),
        _ => "google".to_owned(),
    }
}

fn seat(n: u32) -> SeatId {
    SeatId::numbered(n)
}

/// Opus 5.5 · 1 keeping the notes, Opus 5.5 · 2 the skeptic, and 5.6 Luna.
fn relay_of(listed: &[(&str, SeatRole)], request: &str) -> Relay {
    let listed: Vec<CouncilSeat> = listed
        .iter()
        .map(|(model, role)| CouncilSeat {
            model: model.parse().unwrap(),
            role: *role,
        })
        .collect();
    let names = DisplayNames::default();
    let sitting = seats::seat_listed(&listed, &names).unwrap();
    let notes = seats::note_keeper(&sitting, 0);
    let ids: Vec<SeatId> = sitting.iter().map(|seat| seat.id.clone()).collect();
    let models: Vec<ModelRef> = sitting.iter().map(|seat| seat.model.clone()).collect();
    let key = Draw {
        secret: "made-up-secret",
        run: "run-1",
        council: "wf-001/1",
    };
    let letters = letters::draw(&key, &ids);
    let lexicon = Lexicon::built_in()
        .with_models(&models, &names, &provider)
        .with_marks(CLAUDE_MARKS)
        .with_marks(CODEX_MARKS)
        .with_topic(request);
    let seating = Seating {
        seats: sitting,
        notes,
    };
    Relay::new(seating, letters, lexicon, names)
}

fn council() -> Relay {
    relay_of(
        &[
            (OPUS, SeatRole::Proposer),
            (OPUS, SeatRole::Skeptic),
            (LUNA, SeatRole::Proposer),
        ],
        "Add rate limiting to the public API",
    )
}

fn letter(relay: &Relay, reader: u32, of: u32) -> String {
    relay
        .letters(&seat(reader))
        .and_then(|letters| letters.of(&seat(of)))
        .unwrap()
        .to_string()
}

fn message(from: Speaker, text: Vec<TextPart>) -> CouncilMessage {
    CouncilMessage {
        id: MessageId::numbered(1),
        from,
        kind: MessageKind::Talk,
        text,
        reply_to: None,
        mentions: Vec::new(),
        stances: Vec::new(),
        hidden: 0,
        hidden_names: Vec::new(),
        guesses_hidden: 0,
        rewritten: false,
        declined: false,
        started_before: None,
    }
}

/// What `reader` reads of a message `writer` wrote as `raw`.
fn read(relay: &Relay, writer: u32, reader: u32, raw: &str) -> String {
    let taken = relay.take_in(&seat(writer), raw).unwrap();
    relay.render_for(
        &seat(reader),
        &message(Speaker::Seat(seat(writer)), taken.text),
    )
}

#[test]
fn names_become_hidden_and_tools_read_in_plain_words() {
    let relay = council();
    assert_eq!(
        read(
            &relay,
            2,
            1,
            "I ran both with Claude Code's Bash tool: the counter takes 0.2 ms."
        ),
        "I ran both with [hidden]'s shell tool: the counter takes 0.2 ms."
    );
    assert_eq!(
        read(&relay, 2, 1, "⏺ Bash(npm test)\n  ⎿ 12 passed"),
        "Ran npm test: 12 passed"
    );
    assert_eq!(
        read(&relay, 3, 1, "• Ran cargo test\n└ ok. 4 passed"),
        "Ran cargo test: ok. 4 passed"
    );
    assert_eq!(
        read(
            &relay,
            3,
            1,
            "See 【F:src/rate/bucket.lua†L3-L9】 and ~/.codex/config.toml."
        ),
        "See src/rate/bucket.lua:3 and [hidden]/config.toml."
    );
}

#[test]
fn a_model_naming_itself_and_signatures_go() {
    let relay = council();
    assert_eq!(
        read(&relay, 1, 3, "As Claude, I'd keep the window."),
        "I'd keep the window."
    );
    assert_eq!(
        read(&relay, 3, 1, "Codex here. The bucket wins at the edge."),
        "The bucket wins at the edge."
    );
    assert_eq!(
        read(
            &relay,
            3,
            1,
            "I'm an AI model trained by OpenAI. The bucket wins.\n\n— GPT-5.6"
        ),
        "The bucket wins."
    );
    assert_eq!(
        read(
            &relay,
            1,
            2,
            "The bucket wins.\n\nGenerated with Claude Code\nCo-Authored-By: Someone <someone@example.com>"
        ),
        "The bucket wins."
    );
}

#[test]
fn code_is_never_renamed_and_topic_words_stay() {
    let relay = relay_of(
        &[(OPUS, SeatRole::Proposer), (LUNA, SeatRole::Proposer)],
        "Fix the retries in the Codex adapter",
    );
    assert_eq!(
        read(
            &relay,
            1,
            2,
            "The Codex adapter retries twice; `claude --resume` and\n```\nmodel = \"opus\"\n```\nstay."
        ),
        "The Codex adapter retries twice; `claude --resume` and\n```\nmodel = \"opus\"\n```\nstay."
    );
}

#[test]
fn a_topic_word_still_goes_when_a_seat_names_itself_or_another_by_it() {
    let relay = relay_of(
        &[
            (OPUS, SeatRole::Proposer),
            (LUNA, SeatRole::Proposer),
            (OPUS, SeatRole::Skeptic),
        ],
        "Fix the retries in the Codex adapter",
    );
    assert_eq!(
        read(
            &relay,
            2,
            1,
            "As Codex, I'd retry twice in the Codex adapter."
        ),
        "I'd retry twice in the Codex adapter."
    );
    let t = letter(&relay, 1, 2);
    let guessed = relay
        .take_in(
            &seat(1),
            &format!("Seat {t} sounds like a Codex. Seat {t}'s Codex adapter retries twice."),
        )
        .unwrap();
    assert_eq!((guessed.guesses_hidden, guessed.hidden), (1, 0));
    let read = relay.render_for(&seat(3), &message(Speaker::Seat(seat(1)), guessed.text));
    assert!(read.contains("sounds like a [hidden]."), "{read}");
    assert!(read.contains("'s Codex adapter retries twice."), "{read}");
}

/// A topic word still goes where it says what the writer or a seat is, and only the clause
/// that says so goes, never the rest of the message.
#[test]
fn a_topic_word_that_says_who_someone_is_goes_and_the_rest_stays() {
    let relay = relay_of(
        &[
            (OPUS, SeatRole::Proposer),
            (LUNA, SeatRole::Proposer),
            (OPUS, SeatRole::Skeptic),
        ],
        "Add retries to the Claude and OpenAI SDK clients",
    );
    let y = letter(&relay, 1, 3);
    for (raw, reads) in [
        (
            "As a Claude model, I'd keep the window.",
            "I'd keep the window.",
        ),
        ("I'm a Claude model and I'd keep it.", "I'd keep it."),
        ("Being trained by OpenAI, I'd keep it.", "I'd keep it."),
        (
            "I am an AI assistant built by Anthropic, and I'd keep it.",
            "I'd keep it.",
        ),
        ("I'd keep it.\n\nBest,\nClaude", "I'd keep it."),
        ("I'd keep it.\n(Sent from Codex CLI)", "I'd keep it."),
    ] {
        assert_eq!(read(&relay, 1, 2, raw), reads, "{raw:?}");
    }
    for raw in [
        "We Claude models tend to keep it.".to_owned(),
        format!("Seat {y} is clearly a Claude model."),
        format!("I bet Seat {y} runs on OpenAI."),
        format!("Seat {y}, as a fellow Claude, agrees."),
    ] {
        let read = read(&relay, 1, 2, &raw);
        assert!(
            !read.contains("Claude") && !read.contains("OpenAI"),
            "{raw:?} reads {read:?}"
        );
    }
    let topic = read(&relay, 1, 2, "I'd retry the Claude SDK client twice.");
    assert_eq!(topic, "I'd retry the Claude SDK client twice.");
}

#[test]
fn light_normalising_drops_emphasis_emoji_and_curly_marks() {
    let relay = council();
    assert_eq!(
        read(
            &relay,
            1,
            2,
            "## Plan\n**The bucket** wins — it\u{2019}s *one* call 🚀"
        ),
        "Plan\nThe bucket wins - it's one call"
    );
}

#[test]
fn seats_go_in_by_the_writers_letters_and_out_by_the_readers() {
    let relay = council();
    let t = letter(&relay, 1, 2);
    let m = letter(&relay, 1, 3);
    let raw =
        format!("Seat {t}'s Lua script works. @{m}, does yours take the burst from the plan?");
    let taken = relay.take_in(&seat(1), &raw).unwrap();
    assert_eq!(taken.mentions, [seat(3)]);
    let as_luna = relay.render_for(
        &seat(3),
        &message(Speaker::Seat(seat(1)), taken.text.clone()),
    );
    assert_eq!(
        as_luna,
        format!(
            "Seat {}'s Lua script works. @Seat {}, does yours take the burst from the plan?",
            letter(&relay, 3, 2),
            letter(&relay, 3, 3)
        )
    );
    // Letters are the reader's own, so one seat's T is never another's.
    assert_ne!(
        (letter(&relay, 1, 2), letter(&relay, 1, 3)),
        (letter(&relay, 3, 2), letter(&relay, 3, 3))
    );
}

#[test]
fn a_letter_no_seat_has_is_refused_once_and_a_bare_letter_is_left_alone() {
    let relay = council();
    let used: Vec<String> = (1..=3).map(|n| letter(&relay, 1, n)).collect();
    let free = SeatLetter::POOL
        .iter()
        .find(|letter| !used.contains(&letter.to_string()))
        .unwrap();
    let refused = relay
        .take_in(&seat(1), &format!("Seat {free} is wrong."))
        .unwrap_err();
    assert_eq!(refused.letter, *free);
    assert!(
        refused
            .ask
            .starts_with(&format!("There is no Seat {free} in this council"))
    );
    let bare = relay.take_in(&seat(1), &format!("Set {free} = 3")).unwrap();
    assert_eq!(bare.text, [TextPart::Words(format!("Set {free} = 3"))]);
}

const FLASH: &str = "antigravity-cli/gemini-3.8-flash";

/// Opus 5.5, 5.6 Luna and 3.8 Flash.
fn three_makers() -> Relay {
    relay_of(
        &[
            (OPUS, SeatRole::Proposer),
            (LUNA, SeatRole::Proposer),
            (FLASH, SeatRole::Proposer),
        ],
        "Add rate limiting to the public API",
    )
}

/// The names a screen shows after a logo, a model's own word, a variant after its name, and a
/// name spaced, hyphenated or split by marks a reader doesn't see are all names.
#[test]
fn every_form_of_a_models_name_is_hidden() {
    let relay = three_makers();
    for raw in [
        "5.6 Luna would keep it.",
        "3.8 Flash would keep it.",
        "Luna here, and I'd keep it.",
        "GPT-5.6 Luna would keep it.",
        "Ask gpt-5.6-luna-high to keep it.",
        "DeepSeek-R1 would keep it.",
        "Kimi-K2 would keep it.",
        "Opus\u{a0}5.5 would keep it.",
        "GPT\u{2011}5.6 would keep it.",
        "ClaudeCode shows it, and claude_code agrees.",
        "Cl\u{200b}aude would keep it.",
    ] {
        let read = read(&relay, 1, 2, raw);
        assert!(relay.leaks(&read).is_empty(), "{raw:?} reads {read:?}");
        for left in [
            "Luna", "Flash", "-high", "-R1", "-K2", "5.5", "5.6", "Claude", "claude",
        ] {
            assert!(!read.contains(left), "{raw:?} reads {read:?}");
        }
    }
}

/// Code keeps a name as written, but what only a CLI writes goes from it too: its home
/// folders, its own tools, its transcript rows, and a model naming itself in a code span.
#[test]
fn code_loses_a_clis_marks_and_a_model_naming_itself() {
    let relay = council();
    let cases = [
        (
            "I checked `~/.claude/settings.json` and ran `TodoWrite`.",
            &["~/.claude", "TodoWrite"][..],
        ),
        (
            "I ran `apply_patch` and `update_plan`, see `~/.codex/config.toml`.",
            &["apply_patch", "update_plan", "~/.codex"][..],
        ),
        (
            "```\n⏺ Bash(npm test)\n  ⎿  12 passed\n```",
            &["⏺", "⎿", "Bash("][..],
        ),
        (
            "I'm `claude-opus-5-5`, and I'd keep the window.",
            &["claude-opus-5-5", "I'm"][..],
        ),
    ];
    for (raw, gone) in cases {
        let read = read(&relay, 1, 2, raw);
        for left in gone {
            assert!(!read.contains(left), "{raw:?} reads {read:?}");
        }
    }
    let kept = read(&relay, 1, 2, "We pin `gpt-5.6-luna` in the config.");
    assert!(kept.contains("`gpt-5.6-luna`"), "{kept}");
    let read = read(
        &relay,
        1,
        2,
        "I'm `claude-opus-5-5`, and I'd keep the window.",
    );
    assert!(read.contains("keep the window"), "{read}");
}

/// A model's own word you write names its seat, as its maker's word does.
#[test]
fn your_word_for_a_model_names_its_seat() {
    let relay = three_makers();
    let parts = relay.from_you("I agree with Luna and Flash.").unwrap();
    let seats: Vec<&SeatId> = parts
        .iter()
        .filter_map(|part| match part {
            TextPart::Seat(seat) => Some(seat),
            TextPart::Words(_) => None,
        })
        .collect();
    assert_eq!(seats, [&seat(2), &seat(3)], "{parts:?}");
}

/// A seat that quotes another's words as written, not as it read them, guesses at them, and a
/// match would confirm the guess.
#[test]
fn a_quote_of_words_no_seat_read_matches_nothing() {
    let relay = council();
    let signed = relay
        .take_in(
            &seat(1),
            "As Claude, I'd keep the window. Claude Code's Bash tool agrees.",
        )
        .unwrap();
    let signed = message(Speaker::Seat(seat(1)), signed.text);
    let guess = relay.quote_in(&seat(2), "As Claude").unwrap();
    assert!(!relay.quote_matches(&guess, &signed));
    let guess = relay.quote_in(&seat(2), "Claude Code's Bash tool").unwrap();
    assert!(!relay.quote_matches(&guess, &signed));
}

/// However a quote came to hold names, each reader reads it stripped like the message itself.
#[test]
fn a_reply_quotes_a_seats_words_stripped_for_its_reader() {
    let relay = council();
    let first = relay
        .take_in(&seat(1), "As Claude, I'd keep the window.")
        .unwrap();
    let mut first = message(Speaker::Seat(seat(1)), first.text);
    first.id = MessageId::numbered(1);
    let mut reply = message(Speaker::You, vec![TextPart::Words("Why?".to_owned())]);
    reply.id = MessageId::numbered(2);
    reply.reply_to = Some(ReplyTo {
        message: first.id.clone(),
        quote: "As Claude, I'd keep the window.\n\nCo-Authored-By: Claude Opus 5.5 \
                <noreply@anthropic.com>"
            .to_owned(),
    });
    for reader in [2, 3] {
        let read = relay.turn_line(&seat(reader), &reply, Some(&first));
        assert!(relay.leaks(&read).is_empty(), "{read}");
        assert!(
            !read.contains("Claude") && !read.contains("noreply"),
            "{read}"
        );
    }
}

#[test]
fn a_quote_matches_word_for_word_whoever_reads_it() {
    let relay = council();
    let t = letter(&relay, 1, 2);
    let original = relay
        .take_in(&seat(1), &format!("Seat {t}'s Lua script takes 0.4 ms"))
        .unwrap();
    let original = message(Speaker::Seat(seat(1)), original.text);
    // Luna read it with its own letter for Opus 5.5 · 2, and quotes it so.
    let quoted = format!("Seat {}'s Lua script", letter(&relay, 3, 2));
    let quote = relay.quote_in(&seat(3), &quoted).unwrap();
    assert!(relay.quote_matches(&quote, &original));
    let changed = relay
        .quote_in(&seat(3), "the Lua script takes 0.3 ms")
        .unwrap();
    assert!(!relay.quote_matches(&changed, &original));
    // A quote of what the seat read, names hidden, matches too.
    let named = relay
        .take_in(&seat(2), "I ran both with Claude Code's Bash tool")
        .unwrap();
    let named = message(Speaker::Seat(seat(2)), named.text);
    let read_quote = relay.quote_in(&seat(1), "[hidden]'s shell tool").unwrap();
    assert!(relay.quote_matches(&read_quote, &named));
}

#[test]
fn too_much_hidden_asks_once_and_a_second_try_that_still_names_goes_as_codes_rewrite() {
    let relay = council();
    let raw = "Claude, GPT, Gemini and Qwen all agree.";
    let first = relay.take_in(&seat(1), raw).unwrap();
    assert_eq!(first.hidden, 4);
    assert_eq!(first.ask.as_deref(), Some(REWRITE_ASK));
    let again = relay.take_again(&seat(1), raw).unwrap();
    assert!(again.rewritten);
    assert_eq!(again.ask, None);
    let fine = relay.take_in(&seat(1), "The bucket wins.").unwrap();
    assert_eq!((fine.hidden, fine.ask, fine.rewritten), (0, None, false));
}

#[test]
fn each_hidden_name_says_what_it_named() {
    let relay = council();
    let taken = relay
        .take_in(&seat(1), "I ran both with Claude Code's shell tool.")
        .unwrap();
    assert_eq!((taken.hidden, taken.kinds), (1, vec![HiddenName::Cli]));
    let taken = relay.take_in(&seat(1), "Gemini agrees.").unwrap();
    assert_eq!(taken.hidden, 1);
    assert_ne!(
        taken.kinds,
        vec![HiddenName::Cli],
        "a model or its maker, not a CLI"
    );
}

#[test]
fn a_guess_at_who_runs_a_seat_is_hidden_and_counted_as_a_guess() {
    let relay = council();
    let t = letter(&relay, 1, 2);
    let taken = relay
        .take_in(
            &seat(1),
            &format!("Seat {t} sounds like GPT. The bucket wins."),
        )
        .unwrap();
    assert_eq!((taken.guesses_hidden, taken.hidden), (1, 0));
    let read = relay.render_for(&seat(3), &message(Speaker::Seat(seat(1)), taken.text));
    assert!(read.contains("sounds like [hidden]"), "{read}");
}

#[test]
fn a_long_message_is_asked_to_be_shorter_once_and_capped_after() {
    let relay = council();
    let long = "The bucket wins at the edge. ".repeat(60);
    let taken = relay.take_in(&seat(1), &long).unwrap();
    assert_eq!(taken.ask.as_deref(), Some(SHORTEN_ASK));
    let read = relay.render_for(&seat(2), &message(Speaker::Seat(seat(1)), taken.text));
    assert!(read.chars().count() <= MESSAGE_CHARS);
    assert!(read.ends_with('…'));
}

#[test]
fn your_seat_names_reach_each_seat_as_its_letters() {
    let relay = council();
    let parts = relay
        .from_you("@Opus 5.5 · 2, and Opus 5.5 · 1 too: take the burst from the plan")
        .unwrap();
    let yours = message(Speaker::You, parts);
    assert_eq!(
        relay.render_for(&seat(1), &yours),
        format!(
            "@Seat {}, and Seat {} (you) too: take the burst from the plan",
            letter(&relay, 1, 2),
            letter(&relay, 1, 1)
        )
    );
    let bare = relay.from_you("Opus is right here").unwrap_err();
    assert_eq!(bare.seats, [seat(1), seat(2)]);
    assert!(relay.from_you("Luna is right").is_ok());
    assert_eq!(
        relay.read_as_written("Ask Google how Gemini does it"),
        ["Google", "Gemini"]
    );
}

#[test]
fn turn_lines_name_the_writer_whom_it_answers_and_the_tapbacks() {
    let relay = council();
    let first = relay
        .take_in(&seat(1), "A token bucket per API key, in Redis")
        .unwrap();
    let mut first = message(Speaker::Seat(seat(1)), first.text);
    first.id = MessageId::numbered(1);
    let reply = relay
        .take_in(&seat(2), "A bucket needs a burst size.")
        .unwrap();
    let mut reply = message(Speaker::Seat(seat(2)), reply.text);
    reply.id = MessageId::numbered(2);
    reply.reply_to = Some(ReplyTo {
        message: first.id.clone(),
        quote: relay
            .quote_in(&seat(2), "A token bucket per API key")
            .unwrap(),
    });
    let k = letter(&relay, 1, 2);
    assert_eq!(
        relay.turn_line(&seat(1), &reply, Some(&first)),
        format!(
            "From the council · Seat {k}, replying to you (\"A token bucket per API key\"): A \
             bucket needs a burst size."
        )
    );
    assert_eq!(
        relay.turn_line(&seat(3), &reply, Some(&first)),
        format!(
            "From the council · Seat {}, replying to Seat {} (\"A token bucket per API key\"): A \
             bucket needs a burst size.",
            letter(&relay, 3, 2),
            letter(&relay, 3, 1)
        )
    );
    let tapbacks = [(seat(3), Stance::Object), (seat(1), Stance::Object)];
    assert_eq!(
        relay.tapbacks_line(&seat(1), &reply, &tapbacks),
        format!(
            "From the council · Tapbacks on Seat {k}'s message: Seat {} ✘ · yours ✘",
            letter(&relay, 1, 3)
        )
    );
    let mut declined = reply.clone();
    declined.declined = true;
    assert_eq!(
        relay.turn_line(&seat(1), &declined, None),
        format!("From the council · Seat {k} declined to answer this point")
    );
}

/// A turn is the council's lines in each reader's letters, then the seat's move, and nothing
/// that says how a seat works: no times, no token or tool counts, no read receipts.
#[test]
fn a_turn_is_its_lines_in_the_readers_letters_then_the_move() {
    let relay = council();
    let yours = message(
        Speaker::You,
        vec![TextPart::Words("Keep the limit per API key.".to_owned())],
    );
    let said = relay
        .take_in(&seat(2), "Agreed, Claude's window resets too late.")
        .unwrap();
    let said = message(Speaker::Seat(seat(2)), said.text);
    let point = Point {
        number: 4,
        words: "A token bucket, as [seat-002] put it".to_owned(),
        state: PointState::Agreed,
        raised_by: None,
        sides: Vec::new(),
    };
    let lines = [
        relay.turn_line(&seat(1), &yours, None),
        relay.turn_line(&seat(1), &said, None),
        relay.agreed_line(&seat(1), &point, 4, 6),
    ];
    let k = letter(&relay, 1, 2);
    assert_eq!(
        lines,
        [
            "From the council · the user: Keep the limit per API key.".to_owned(),
            format!("From the council · Seat {k}: Agreed, [hidden]'s window resets too late."),
            format!(
                "From the council · Point 4 agreed: A token bucket, as Seat {k} put it · 4 of 6 \
                 points agreed"
            ),
        ]
    );
    assert_eq!(
        relay.agreed_line(&seat(3), &point, 4, 6),
        format!(
            "From the council · Point 4 agreed: A token bucket, as Seat {} put it · 4 of 6 points \
             agreed",
            letter(&relay, 3, 2)
        ),
        "each reader reads the point in its own letters"
    );
    for (your_move, words) in [
        (YourMove::Write, "write your message"),
        (
            YourMove::React,
            "react with a stance on each point it touches",
        ),
        (YourMove::Wait, "nothing yet"),
    ] {
        assert_eq!(
            turn(&lines, your_move),
            format!("{}\nYour move: {words}\n", lines.join("\n"))
        );
    }
    let read = turn(&lines, YourMove::Write);
    let clock = Regex::new(r"[0-9]{1,2}:[0-9]{2}|\b(AM|PM)\b").unwrap();
    assert!(!clock.is_match(&read), "no times: {read}");
    for told in ["tokens", "tool call", "Read by", "Seen"] {
        assert!(!read.contains(told), "no {told:?} in a turn: {read}");
    }
}

#[test]
fn a_refusal_is_read_as_declining_to_answer() {
    let relay = council();
    let taken = relay
        .take_in(&seat(3), "I can't help with that part of the request.")
        .unwrap();
    assert!(taken.declined);
}

/// The frame changes only in its letters and count, says nothing of what runs any seat, and
/// tells every seat any other may run its own model.
#[test]
fn the_frame_is_the_same_for_three_opus_seats_and_a_mixed_council() {
    let three_opus = relay_of(
        &[
            (OPUS, SeatRole::Proposer),
            (OPUS, SeatRole::Skeptic),
            (OPUS, SeatRole::Proposer),
        ],
        "Add rate limiting",
    );
    let mixed = relay_of(
        &[
            (OPUS, SeatRole::Proposer),
            (LUNA, SeatRole::Skeptic),
            ("antigravity-cli/gemini-3.8-flash", SeatRole::Proposer),
        ],
        "Add rate limiting",
    );
    let unlettered = |relay: &Relay, reader: u32| {
        let frame = relay.frame_for(&seat(reader));
        let letters = relay.letters(&seat(reader)).unwrap();
        let mut frame = frame;
        for n in 1..=3 {
            let letter = letters.of(&seat(n)).unwrap();
            frame = frame.replace(&format!("Seat {letter}"), "Seat ?");
            frame = frame.replace(&format!("@{letter} "), "@? ");
        }
        frame
    };
    for reader in 1..=3 {
        assert_eq!(unlettered(&three_opus, reader), unlettered(&mixed, reader));
        for relay in [&three_opus, &mixed] {
            let frame = relay.frame_for(&seat(reader));
            assert!(relay.leaks(&frame).is_empty(), "{frame}");
            assert!(frame.contains("Any seat may run the same model as you, or a different one"));
        }
    }
    let skeptic = three_opus.frame_for(&seat(2));
    assert!(skeptic.contains("Your role: skeptic"));
    assert!(
        !skeptic.contains("is the skeptic."),
        "a skeptic isn't told about itself"
    );
    let keeper = three_opus.frame_for(&seat(1));
    assert!(keeper.contains("You also keep the notes"));
    assert!(keeper.contains(&format!(
        "Seat {} is the skeptic.",
        letter(&three_opus, 1, 2)
    )));
    assert!(!keeper.contains("\n\n\n"));
}

#[test]
fn a_plan_hides_who_wrote_it_and_keeps_its_markdown() {
    let relay = council();
    let plan = "## Summary\n\nAs Claude, I'd add a token bucket.\n\n- **Paths**: `src/rate.rs`\n";
    assert_eq!(
        hide_who_wrote(&relay.lexicon, plan),
        "## Summary\n\nI'd add a token bucket.\n\n- **Paths**: `src/rate.rs`"
    );
}
