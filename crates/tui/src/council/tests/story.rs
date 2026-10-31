//! The made-up council the goldens draw: four seats rate limit a shop's public API. Opus 5.5 on
//! Claude Code keeps the notes, GPT 5.6 Luna sits on Codex, Gemini 3.8 Flash on agy and Qwen3 Coder 30B on
//! OpenCode with a local model.

use argonauts_core::TaskId;
use argonauts_core::base::names::{AgentId, MessageId, SeatId, SeatLetter, WorkflowId};
use argonauts_core::look::logos::{Logo, LogoMark};
use argonauts_core::runs::events::approval::WaysOffer;
use argonauts_core::runs::events::task::Way;
use argonauts_core::workflows::workflow::WorkflowRef;

use argonauts_core::runs::events::council::CheckRung;
use argonauts_core::workflows::steps::judgment::ReviewVerdict;

use crate::council::screens::edits::{EditSheet, EditTask};
use crate::council::screens::session_view::{SeatSession, Turn};
use crate::council::screens::thread::Thread;
use crate::council::talk::faces::{Face, Faces};
use crate::council::talk::item::{Body, Header, Inset, Item, Quote, SeatSays, YouSay};
use crate::council::talk::words::{Mark, Piece, Tone};
use crate::council::work::checks::{self, Approved, Checked, Checker};
use crate::council::work::plan::{
    Columns, Now, PlanTask, Progress, Step, TaskWork, Who, you_asked,
};
use crate::council::work::task_header::TaskHeader;

pub fn opus() -> SeatId {
    SeatId::numbered(1)
}

pub fn luna() -> SeatId {
    SeatId::numbered(2)
}

pub fn flash() -> SeatId {
    SeatId::numbered(3)
}

pub fn qwen() -> SeatId {
    SeatId::numbered(4)
}

pub fn all() -> Vec<SeatId> {
    vec![opus(), luna(), flash(), qwen()]
}

fn logo(agent: &str, mark: LogoMark) -> Logo {
    Logo {
        agent: AgentId(agent.to_owned()),
        mark: Some(mark),
    }
}

pub fn faces() -> Faces {
    Faces::new([
        (
            opus(),
            Face::new(
                "Opus 5.5",
                "claude-code",
                logo("claude-code", LogoMark::ClaudeCode),
            ),
        ),
        (
            luna(),
            Face::new("GPT 5.6 Luna", "codex", logo("codex", LogoMark::Codex)),
        ),
        (
            flash(),
            Face::new(
                "Gemini 3.8 Flash",
                "antigravity-cli",
                logo("antigravity-cli", LogoMark::Antigravity),
            ),
        ),
        (
            qwen(),
            Face::new(
                "Qwen3 Coder 30B",
                "opencode",
                logo("opencode", LogoMark::Ollama),
            ),
        ),
    ])
}

/// Words with seats named in braces, `{flash}`, as pieces.
pub fn said(text: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let close = rest[open..].find('}').map_or(rest.len(), |i| open + i);
        if open > 0 {
            pieces.push(Piece::text(&rest[..open]));
        }
        pieces.push(Piece::Seat(seat_named(&rest[open + 1..close])));
        rest = &rest[(close + 1).min(rest.len())..];
    }
    if !rest.is_empty() {
        pieces.push(Piece::text(rest));
    }
    pieces
}

pub fn seat_named(name: &str) -> SeatId {
    match name {
        "opus" => opus(),
        "luna" => luna(),
        "flash" => flash(),
        "qwen" => qwen(),
        "opus_one" => opus_one(),
        "opus_two" => opus_two(),
        other => panic!("no seat {other}"),
    }
}

pub fn says(seat: SeatId, paragraphs: &[&str]) -> SeatSays {
    SeatSays::new(
        seat,
        Body::words(paragraphs.iter().map(|text| said(text)).collect()),
    )
}

pub fn line(text: &str) -> Item {
    Item::Line(vec![Piece::dim(text)])
}

pub fn quote(seat: Option<SeatId>, words: &str, replies: u32) -> Option<Quote> {
    Some(Quote {
        seat,
        words: words.to_owned(),
        replies,
    })
}

pub fn taps(marks: &[(SeatId, Mark)]) -> Vec<(SeatId, Mark)> {
    marks.to_vec()
}

pub const PROMPT: &str = "Add rate limiting to the public API, so one noisy client can't slow down \
    everyone else. Build the limiter with Maker-checker, split the route tests across agents, and \
    let Luna do the plan limits in its own session.";

pub fn header() -> Item {
    Item::Header(Header {
        seats: all(),
        title: "Council".to_owned(),
        subtitle: "4 seats · each one is its own session · Opus 5.5 keeps the notes".to_owned(),
    })
}

pub fn prompt() -> Item {
    Item::You(YouSay {
        paragraphs: vec![said(PROMPT)],
        read_by: all(),
        ..YouSay::default()
    })
}

pub fn alone_first() -> Item {
    line("Each seat answers alone first, without seeing the others")
}

/// A first answer: its opening and two points show, the rest folds. The folded rows are made-up
/// filler as long as the answer's rest.
fn first_answer(seat: SeatId, shown: [&str; 3], folded: usize, did: &str) -> Item {
    let mut paragraphs: Vec<Vec<Piece>> = shown.iter().map(|text| said(text)).collect();
    paragraphs.extend(
        (0..folded).map(|i| vec![Piece::text(format!("- More of the answer, row {}", i + 1))]),
    );
    let mut message = SeatSays::new(
        seat,
        Body::Words {
            paragraphs,
            shown: Some(3),
        },
    );
    message.right = Some(did.to_owned());
    Item::Seat(vec![message])
}

pub fn first_answers() -> Vec<Item> {
    vec![
        first_answer(
            luna(),
            [
                "A sliding window counter per key in Redis: INCR and EXPIRE, one call, nothing to tune.",
                "- Limits from the key's plan in the keys table: free 60 a minute, pro 600",
                "- Calls with no key get 401; the docs already say every call needs one",
            ],
            7,
            "Read 3 files · ran 1 shell command",
        ),
        first_answer(
            opus(),
            [
                "A token bucket per API key, in Redis: we run 3 API nodes, so a per-node limit lets a client in 3 times over.",
                "- 100 at once, refilled at 10 a second; an empty bucket answers 429 with Retry-After",
                "- Calls with no key share one small bucket, so /health and the status page keep working",
            ],
            9,
            "Read 5 files · ran 2 shell commands",
        ),
        first_answer(
            flash(),
            [
                "A token bucket per key in Redis, checked and taken in one Lua script, so two nodes can't both take the last token.",
                "- The importer key sends about 2,000 a minute at night (logs/importer.log)",
                "- Send X-RateLimit-Remaining on every answer, so clients slow down before a 429",
            ],
            5,
            "Read 6 files · ran 3 shell commands",
        ),
        first_answer(
            qwen(),
            [
                "An in-memory bucket on each node, behind a RateStore interface; Redis can come later.",
                "- No new dependency, and the tests need no Redis",
                "- Give each node a third of the limit, since there are 3 nodes",
            ],
            4,
            "Read 4 files",
        ),
    ]
}

pub fn everyone_answered() -> Item {
    line("Everyone answered · Opus 5.5 keeps the notes and gathers the points")
}

fn side(text: &str, seats: &[SeatId]) -> Vec<Piece> {
    let mut pieces = vec![Piece::text(format!("{text} "))];
    for (i, seat) in seats.iter().enumerate() {
        if i > 0 {
            pieces.push(Piece::dim(" "));
        }
        pieces.push(Piece::Badge(seat.clone()));
    }
    pieces
}

fn open_point(first: (&str, &[SeatId]), second: (&str, &[SeatId])) -> Vec<Piece> {
    let mut row = vec![Piece::tone("?  ", Tone::WarnBold)];
    row.extend(side(first.0, first.1));
    row.push(Piece::dim("   or   "));
    row.extend(side(second.0, second.1));
    row
}

pub fn proposal_rows() -> Vec<Vec<Piece>> {
    let agreed = |text: &str| vec![Piece::tone("✔  ", Tone::Ok), Piece::text(text)];
    vec![
        vec![
            Piece::tone("Proposal", Tone::Bold),
            Piece::dim(" · 6 points from 4 answers · 3 ways from you"),
        ],
        agreed("Limits from the key's plan: free 60 a minute, pro 600; importer 2,000"),
        agreed("An empty bucket answers 429 with Retry-After and X-RateLimit headers"),
        agreed("Tests use an in-memory store behind a RateStore interface"),
        open_point(
            ("A token bucket", &[opus(), flash(), qwen()]),
            ("a window counter", &[luna()]),
        ),
        open_point(
            ("Kept in Redis", &[opus(), luna(), flash()]),
            ("in memory on each node", &[qwen()]),
        ),
        open_point(
            ("Calls with no key: one shared bucket", &[opus(), flash()]),
            ("401", &[luna(), qwen()]),
        ),
        vec![
            Piece::tone("❯  ", Tone::Yours),
            Piece::tone("Yours, binding: ", Tone::Yours),
            Piece::text("the limiter by Maker-checker · the route tests across agents"),
        ],
        vec![
            Piece::text("   the plan limits by "),
            Piece::Seat(luna()),
            Piece::text(" in its own session"),
        ],
    ]
}

pub fn proposal() -> Item {
    let mut message = says(
        opus(),
        &["Where our four answers meet: 3 points agreed, 3 still open."],
    );
    message.note = Some(" · keeps the notes".to_owned());
    message.inset = Some(Inset::Rows(proposal_rows()));
    Item::Seat(vec![message])
}

pub fn talk_until_agreed() -> Item {
    line("3 of 6 points agreed · they talk until every seat agrees")
}

pub fn luna_on_buckets(with_taps: bool) -> Item {
    let mut message = says(
        luna(),
        &[
            "A bucket has two numbers to tune, burst and refill. A window counter is one INCR and one EXPIRE, and nobody has to pick a burst size.",
        ],
    );
    message.reply = quote(Some(opus()), "A token bucket per API key, in Redis", 0);
    if with_taps {
        message.taps = taps(&[
            (opus(), Mark::Object),
            (flash(), Mark::Object),
            (qwen(), Mark::Object),
        ]);
    }
    Item::Seat(vec![message])
}

pub fn opus_on_edges() -> Item {
    let mut message = says(
        opus(),
        &[
            "A window counter lets a client send twice its limit across a window's edge. With {flash}'s Lua script a bucket is still one Redis call, and its burst is the plan's limit, so there's nothing new to tune.",
        ],
    );
    message.reply = quote(
        Some(luna()),
        "A window counter is one INCR and one EXPIRE",
        0,
    );
    message.taps = taps(&[
        (luna(), Mark::Agree),
        (flash(), Mark::Agree),
        (qwen(), Mark::Agree),
    ]);
    Item::Seat(vec![message])
}

pub fn luna_moved(wide: bool) -> Item {
    if !wide {
        return Item::Line(vec![
            Piece::Called(luna(), Tone::Dim),
            Piece::dim(" moved to a token bucket · 4 of 6 agreed"),
        ]);
    }
    Item::Line(vec![
        Piece::Called(luna(), Tone::Dim),
        Piece::dim(" moved to a token bucket: "),
        Piece::tone("\"twice the limit at a window's edge\"", Tone::Quote),
        Piece::dim(" · 4 of 6 points agreed"),
    ])
}

pub const FLASH_ON_NODES: &str = "The balancer spreads a client over all 3 nodes, so a third each \
    still lets bursts through. Redis is already here for sessions (src/session/store.ts); keep \
    your interface for the tests. And the balancer's probe calls /health with no key, so a plain \
    401 takes a node out.";

pub fn flash_on_nodes(picked: bool) -> Item {
    let mut message = says(flash(), &[FLASH_ON_NODES]);
    message.reply = quote(Some(qwen()), "An in-memory bucket on each node", 0);
    message.taps = taps(&[(opus(), Mark::Agree), (luna(), Mark::Object)]);
    message.picked = picked;
    Item::Seat(vec![message])
}

/// The talk from the proposal to 3.8 Flash's message on nodes.
pub fn talk(wide: bool, picked: bool) -> Vec<Item> {
    let mut items = Vec::new();
    if wide {
        items.push(proposal());
    }
    items.extend([
        talk_until_agreed(),
        luna_on_buckets(true),
        opus_on_edges(),
        luna_moved(wide),
        flash_on_nodes(picked),
    ]);
    items
}

pub const NO_KEY: &str =
    "Calls with no key: don't break them yet. Warn them for 30 days, then 401. /health stays open.";

pub fn your_reply(read_by_all: bool) -> Item {
    let (read_by, read_after) = if read_by_all {
        (all(), Vec::new())
    } else {
        (
            vec![opus(), luna(), flash()],
            vec![
                Piece::dim("   "),
                Piece::Called(qwen(), Tone::Dim),
                Piece::dim(" reads it after its current message"),
            ],
        )
    };
    Item::You(YouSay {
        paragraphs: vec![said(NO_KEY)],
        reply: quote(
            Some(flash()),
            "And the balancer's probe calls /health with no key",
            1,
        ),
        read_by,
        read_after,
        picked: false,
    })
}

pub fn qwen_agrees(marks: &[(SeatId, Mark)]) -> Item {
    let mut message = says(
        qwen(),
        &[
            "Agreed: Redis for real, the in-memory store for tests. On calls with no key I'm with {luna}: 401.",
        ],
    );
    message.note = Some(" · started before it read yours".to_owned());
    message.reply = quote(
        Some(flash()),
        "The balancer spreads a client over all 3 nodes",
        2,
    );
    message.taps = marks.to_vec();
    Item::Seat(vec![message])
}

pub fn qwen_moved() -> Item {
    Item::Line(vec![
        Piece::Called(qwen(), Tone::Dim),
        Piece::dim(" moved to Redis after reading "),
        Piece::tone("src/session/store.ts", Tone::Quote),
        Piece::dim(" · 5 of 6 points agreed"),
    ])
}

pub const FLASH_ON_DEPRECATION: &str = "Then for 30 days calls with no key keep a small shared \
    bucket, 10 a second, and get a Deprecation header with the date; after that, 401. /health \
    stays open for good.";

pub fn flash_on_deprecation(count: usize) -> Item {
    let mut message = says(flash(), &[FLASH_ON_DEPRECATION]);
    message.reply = quote(None, "Calls with no key: don't break them yet", 3);
    let marks = [
        (opus(), Mark::Agree),
        (luna(), Mark::Agree),
        (qwen(), Mark::Agree),
    ];
    message.taps = marks[..count].to_vec();
    Item::Seat(vec![message])
}

pub fn everyone_agreed() -> Item {
    let mut pieces = vec![
        Piece::tone("✔  ", Tone::Ok),
        Piece::tone("Everyone agreed", Tone::OkBold),
        Piece::dim(" · 6 of 6 points  "),
    ];
    for (i, seat) in all().into_iter().enumerate() {
        if i > 0 {
            pieces.push(Piece::dim(" "));
        }
        pieces.push(Piece::Badge(seat));
    }
    Item::Line(pieces)
}

/// The second seat of Opus 5.5, in the council where one model sits twice.
pub fn opus_two() -> SeatId {
    SeatId::numbered(5)
}

/// Opus 5.5 sits twice, as the skeptic the second time, beside 5.6 Luna and 3.8 Flash.
pub fn faces_one_model_twice() -> Faces {
    let opus = |number| {
        Face::new(
            "Opus 5.5",
            "claude-code",
            logo("claude-code", LogoMark::ClaudeCode),
        )
        .numbered(number)
    };
    let full = faces();
    Faces::new([
        (opus_one(), opus(1)),
        (opus_two(), opus(2)),
        (luna(), full.face(&luna())),
        (flash(), full.face(&flash())),
    ])
}

pub fn opus_one() -> SeatId {
    opus()
}

fn with_taps(mut message: SeatSays, marks: &[(SeatId, Mark)]) -> SeatSays {
    message.taps = marks.to_vec();
    message
}

pub fn one_model_twice_talk(look_has_room: bool) -> Vec<Item> {
    let skeptic = if look_has_room {
        " Opus 5.5 · 2 is the skeptic: it argues the other side on purpose · its tapbacks are its own view"
    } else {
        " Opus 5.5 · 2 is the skeptic: it argues the other side on purpose"
    };
    let mut second = with_taps(
        says(
            opus_two(),
            &[
                "A bucket needs a burst size, and nobody here has picked one. A window counter is one INCR and one EXPIRE. I ran both with Claude Code's Bash tool: the bucket's Lua script takes 0.4 ms a call, the counter 0.2 ms.",
            ],
        ),
        &[
            (opus_one(), Mark::Object),
            (luna(), Mark::Object),
            (flash(), Mark::Object),
        ],
    );
    second.note = Some(" · a CLI's name hidden from the seats".to_owned());
    second.reply = quote(Some(opus_one()), "A token bucket per API key, in Redis", 0);
    let mut first = with_taps(
        says(
            opus_one(),
            &[
                "The burst is the key's plan limit, so there is nothing new to pick. And 0.2 ms saved doesn't pay for a client sending twice its limit at a window's edge: test/edge.test.ts fails with the counter and passes with the bucket. {luna}, does your script take the burst from the plan?",
            ],
        ),
        &[
            (opus_two(), Mark::Agree),
            (luna(), Mark::Agree),
            (flash(), Mark::Agree),
        ],
    );
    first.reply = quote(
        Some(opus_two()),
        "A bucket needs a burst size, and nobody here has picked one",
        0,
    );
    vec![
        Item::Seat(vec![says(
            opus_one(),
            &[
                "A token bucket per API key, in Redis: one Lua script checks the key's bucket and takes a token. The burst is the plan's limit, so nothing new needs tuning.",
            ],
        )]),
        talk_until_agreed(),
        Item::Line(vec![Piece::Badge(opus_two()), Piece::dim(skeptic)]),
        Item::Seat(vec![second]),
        Item::Seat(vec![first]),
        Item::Line(vec![
            Piece::tone("✔  ", Tone::Ok),
            Piece::dim("Token bucket agreed: "),
            Piece::tone("\"test/edge.test.ts fails with the counter\"", Tone::Quote),
            Piece::dim(" · 4 of 6 points agreed"),
        ]),
    ]
}

pub fn luna_moved_in_private() -> Item {
    Item::Line(vec![
        Piece::Called(luna(), Tone::Dim),
        Piece::dim(
            " moved to one shared bucket · its reason came from you, in private · 5 of 6 points agreed",
        ),
    ])
}

pub fn moved_in_private() -> Vec<Item> {
    let mut moved = with_taps(
        says(
            luna(),
            &[
                "I now back one shared bucket for calls with no key, for now. You told me in private why breaking them soon is costly.",
            ],
        ),
        &[
            (opus_one(), Mark::Agree),
            (opus_two(), Mark::Object),
            (flash(), Mark::Agree),
        ],
    );
    moved.reply = quote(
        Some(opus_one()),
        "Calls with no key should share one small bucket",
        1,
    );
    vec![
        Item::Seat(vec![with_taps(
            says(
                luna(),
                &["401 for calls with no key: the docs already say every call needs one."],
            ),
            &[
                (opus_one(), Mark::Object),
                (opus_two(), Mark::Agree),
                (flash(), Mark::Object),
            ],
        )]),
        Item::Seat(vec![with_taps(
            says(
                opus_one(),
                &[
                    "Calls with no key should share one small bucket, 10 a second: the balancer's probe calls /health with no key, and so do the curl examples in our docs.",
                ],
            ),
            &[
                (opus_two(), Mark::Object),
                (luna(), Mark::Object),
                (flash(), Mark::Agree),
            ],
        )]),
        Item::Line(vec![
            Piece::dim("You wrote to "),
            Piece::Called(luna(), Tone::Dim),
            Piece::dim(" in private · 10:46 AM · no other seat sees it"),
        ]),
        Item::Seat(vec![moved]),
        luna_moved_in_private(),
    ]
}

pub const HELD: &str = "A customer depends on it. Acme's contract promises their old SDK keeps \
    working until March, and it calls /v1/status with no key.";

/// The held message's words, faded, with the 17 that match what you wrote in private marked.
pub fn held_words() -> Vec<Piece> {
    HELD.split(' ')
        .enumerate()
        .flat_map(|(i, word)| {
            let tone = if (5..22).contains(&i) {
                Tone::Matching
            } else {
                Tone::Faded
            };
            let gap = if i == 0 { "" } else { " " };
            let gap_tone = if (6..22).contains(&i) {
                Tone::Matching
            } else {
                Tone::Faded
            };
            [Piece::tone(gap, gap_tone), Piece::tone(word, tone)]
        })
        .filter(|piece| !matches!(piece, Piece::Words(words, _) if words.is_empty()))
        .collect()
}

pub fn held() -> Vec<Item> {
    let mut ask = says(
        opus_two(),
        &[
            "One shared bucket is one bucket a single script can use up. If the reason isn't in the repo or the talk, the council can't weigh it. What is it?",
        ],
    );
    ask.reply = quote(
        Some(luna()),
        "I now back one shared bucket for calls with no key",
        0,
    );
    let mut held = SeatSays::new(luna(), Body::words(vec![held_words()]));
    held.held = true;
    held.note = Some(" · held back: no seat has read it".to_owned());
    held.reply = quote(
        Some(opus_two()),
        "If the reason isn't in the repo or the talk",
        0,
    );
    vec![
        luna_moved_in_private(),
        Item::Seat(vec![ask]),
        Item::Seat(vec![held]),
    ]
}

const PLAN: [&str; 6] = [
    "A token bucket per API key in Redis; one Lua script checks and takes a token",
    "Limits from the key's plan: free 60 a minute, pro 600; the importer key 2,000",
    "Empty bucket: 429 with Retry-After; every answer has X-RateLimit-Remaining",
    "No key: a shared bucket and a Deprecation header for 30 days, then 401",
    "/health stays open, with or without a key",
    "The store sits behind a RateStore interface; tests use an in-memory one",
];

pub use crate::council::work::plan::RULE;

pub fn plan_tasks() -> Vec<PlanTask> {
    let task = |name: &str, seat: SeatId, way: &str, why: Vec<Piece>, check: SeatId| PlanTask {
        name: name.to_owned(),
        who: Who::Seat(seat),
        way: way.to_owned(),
        why,
        checked_by: vec![Piece::Badge(check)],
        objection: None,
    };
    let mut limits = task(
        "Plan limits",
        luna(),
        "Own session",
        vec![you_asked()],
        opus(),
    );
    limits.objection = Some((
        flash(),
        "every request reads this table, so Maker-checker".to_owned(),
    ));
    vec![
        task(
            "Redis bucket",
            opus(),
            "Maker-checker",
            vec![you_asked()],
            luna(),
        ),
        limits,
        task(
            "Memory store",
            qwen(),
            "Own session",
            vec![Piece::text("Small, one file")],
            flash(),
        ),
        task(
            "Route tests",
            flash(),
            "3 agents",
            vec![you_asked(), Piece::text(" · 3 files")],
            qwen(),
        ),
    ]
}

fn who_message() -> SeatSays {
    let mut message = says(opus(), &[crate::council::work::plan::PROPOSING]);
    message.inset = Some(Inset::Plan {
        tasks: plan_tasks(),
        columns: Columns::PROPOSED,
        rule: Some(RULE.to_owned()),
        sat_out: Vec::new(),
    });
    message.taps = taps(&[
        (luna(), Mark::Agree),
        (flash(), Mark::Object),
        (qwen(), Mark::Agree),
    ]);
    message
}

/// The plan, then who does what, from the note keeper in a row.
pub fn plan_and_who() -> Item {
    let rows = PLAN
        .iter()
        .enumerate()
        .map(|(i, step)| vec![Piece::dim(format!("{}. ", i + 1)), Piece::text(*step)])
        .collect();
    let mut plan = SeatSays::new(opus(), Body::Rows(rows));
    plan.note = Some(" · the plan".to_owned());
    plan.taps = taps(&[
        (luna(), Mark::Agree),
        (flash(), Mark::Agree),
        (qwen(), Mark::Agree),
    ]);
    Item::Seat(vec![plan, who_message()])
}

pub fn signed() -> Item {
    Item::Line(vec![
        Piece::dim("Every seat signed both · "),
        Piece::Called(flash(), Tone::Dim),
        Piece::dim(" objects to a way you asked for; only you change it"),
    ])
}

fn started(seat: SeatId, what: &str) -> Item {
    Item::Line(vec![Piece::Called(seat, Tone::Dim), Piece::dim(what)])
}

fn progress(done: u32, total: u32, unit: &str, time: &str, tokens: &str) -> Progress {
    Progress {
        done,
        total,
        unit: unit.to_owned(),
        time: time.to_owned(),
        tokens: tokens.to_owned(),
    }
}

pub fn workflow_task(time: &str, tokens: &str) -> TaskWork {
    TaskWork {
        way: "Maker-checker".to_owned(),
        task: "Redis bucket".to_owned(),
        now: Now::Steps {
            steps: vec![
                Step {
                    done: true,
                    name: "Build".to_owned(),
                    by: opus(),
                    note: String::new(),
                },
                Step {
                    done: false,
                    name: "Review".to_owned(),
                    by: luna(),
                    note: ", new session".to_owned(),
                },
            ],
            ending: Some("until it holds".to_owned()),
        },
        progress: progress(1, 2, "steps", time, tokens),
    }
}

pub fn session_task(done: u32, time: &str, tokens: &str) -> TaskWork {
    TaskWork {
        way: "Own session".to_owned(),
        task: "Plan limits".to_owned(),
        now: Now::Call {
            tool: "Bash".to_owned(),
            argument: "npm run migrate:new add_rate_limit_to_plans".to_owned(),
        },
        progress: progress(done, 5, "to-dos", time, tokens),
    }
}

pub fn agents_task(done: u32, time: &str, tokens: &str) -> TaskWork {
    let part = |i: u32, name: &str| (i < done, name.to_owned());
    TaskWork {
        way: "3 agents".to_owned(),
        task: "Route tests".to_owned(),
        now: Now::Agents {
            seat: flash(),
            parts: vec![part(0, "Orders"), part(1, "Users"), part(2, "Import")],
            suffix: " routes".to_owned(),
        },
        progress: progress(done, 3, "agents", time, tokens),
    }
}

fn opus_started() -> Item {
    started(opus(), " started a workflow: Maker-checker")
}

fn flash_started() -> Item {
    started(flash(), " started 3 agents")
}

/// From who does what to the first tasks of the work, with `picked` seat's task picked.
pub fn work_starts(picked: Option<SeatId>) -> Vec<Item> {
    let task = |work: TaskWork, seat: SeatId| {
        let lit = picked.as_ref() == Some(&seat);
        Item::Seat(vec![work.message(seat, lit)])
    };
    vec![
        Item::Seat(vec![who_message()]),
        signed(),
        approved(4, None, &["Plan limits"]),
        opus_started(),
        task(workflow_task("3m36s", "402k"), opus()),
        task(session_task(1, "3m36s", "171k"), luna()),
        flash_started(),
        task(agents_task(0, "1m04s", "52k"), flash()),
    ]
}

pub fn work_nests() -> Vec<Item> {
    let mut done = says(qwen(), &["Memory store is done: 9 tests pass."]);
    done.taps = taps(&[(opus(), Mark::Agree), (flash(), Mark::Agree)]);
    vec![
        opus_started(),
        Item::Seat(vec![workflow_task("6m40s", "612k").message(opus(), false)]),
        Item::Seat(vec![
            session_task(2, "6m40s", "288k").message(luna(), false),
        ]),
        flash_started(),
        Item::Seat(vec![agents_task(1, "4m08s", "164k").message(flash(), true)]),
        Item::Seat(vec![done]),
    ]
    .into_iter()
    .chain(checks::checked(&Checked {
        checker: Checker::Seat(flash()),
        task: "Memory store".to_owned(),
        rung: CheckRung::OtherMaker,
        verdict: ReviewVerdict::Approve,
        maker: "Qwen3 Coder 30B".to_owned(),
        first: true,
    }))
    .collect()
}

/// The plan you approved at 10:51 AM, as the talk sums it up.
fn approved(tasks: usize, checker: Option<(&str, bool)>, kept: &[&str]) -> Item {
    checks::approved(&Approved {
        at: "10:51 AM".to_owned(),
        tasks,
        checker: checker.map(|(name, same)| (name.to_owned(), same)),
        kept: kept.iter().map(|&task| task.to_owned()).collect(),
    })
}

/// Redis bucket's check, the council's first, by a fresh session of `checker`.
fn redis_checked(checker: Checker, rung: CheckRung) -> Vec<Item> {
    checks::checked(&Checked {
        checker,
        task: "Redis bucket".to_owned(),
        rung,
        verdict: ReviewVerdict::Approve,
        maker: "Opus 5.5".to_owned(),
        first: true,
    })
}

pub fn opus_three() -> SeatId {
    SeatId::numbered(6)
}

/// Only Claude Code is set up: Opus 5.5 sits three times.
pub fn faces_one_maker() -> Faces {
    let opus = |number| {
        Face::new(
            "Opus 5.5",
            "claude-code",
            logo("claude-code", LogoMark::ClaudeCode),
        )
        .numbered(number)
    };
    Faces::new([
        (opus_one(), opus(1)),
        (opus_two(), opus(2)),
        (opus_three(), opus(3)),
    ])
}

fn agreed_task(name: &str, seat: SeatId, way: &str, checked_by: Vec<Piece>) -> PlanTask {
    PlanTask {
        name: name.to_owned(),
        who: Who::Seat(seat),
        way: way.to_owned(),
        why: Vec::new(),
        checked_by,
        objection: None,
    }
}

fn who_does_what(tasks: Vec<PlanTask>, marks: &[(SeatId, Mark)]) -> Item {
    let mut message = says(
        opus_one(),
        &["Who does what: each task goes to the seat whose idea it was."],
    );
    message.note = Some(" · keeps the notes".to_owned());
    message.inset = Some(Inset::Plan {
        tasks,
        columns: Columns::AGREED,
        rule: None,
        sat_out: Vec::new(),
    });
    message.taps = marks.to_vec();
    Item::Seat(vec![message])
}

fn made() -> Item {
    Item::Seat(vec![says(
        opus_one(),
        &["Redis bucket is made: one Lua script, 6 files changed, 14 tests pass."],
    )])
}

pub fn blind_check() -> Vec<Item> {
    let flash_checks = || {
        vec![
            Piece::Called(flash(), Tone::Text),
            Piece::text(", fresh session"),
        ]
    };
    vec![
        who_does_what(
            vec![
                agreed_task("Redis bucket", opus_one(), "Maker-checker", flash_checks()),
                agreed_task("Plan limits", luna(), "Own session", flash_checks()),
                agreed_task("Route tests", opus_two(), "3 agents", flash_checks()),
            ],
            &[
                (opus_two(), Mark::Agree),
                (luna(), Mark::Agree),
                (flash(), Mark::Agree),
            ],
        ),
        approved(3, Some(("Gemini 3.8 Flash", false)), &[]),
        started(opus_one(), " started a workflow: Maker-checker"),
        started(opus_two(), " started 3 agents: Route tests"),
        started(luna(), " works on Plan limits in its own session"),
        made(),
    ]
    .into_iter()
    .chain(redis_checked(Checker::Seat(flash()), CheckRung::OtherMaker))
    .collect()
}

pub fn same_maker_check() -> Vec<Item> {
    let fresh = || logo("claude-code", LogoMark::ClaudeCode);
    let same = || {
        vec![
            Piece::Logo(fresh()),
            Piece::text(" Fresh Opus 5.5 · "),
            Piece::tone("Same maker", Tone::Warn),
        ]
    };
    vec![
        who_does_what(
            vec![
                agreed_task("Redis bucket", opus_one(), "Maker-checker", same()),
                agreed_task("Plan limits", opus_three(), "Own session", same()),
                agreed_task("Route tests", opus_two(), "3 agents", same()),
            ],
            &[(opus_two(), Mark::Agree), (opus_three(), Mark::Agree)],
        ),
        approved(3, Some(("Opus 5.5", true)), &[]),
        started(opus_one(), " started a workflow: Maker-checker"),
        started(opus_two(), " started 3 agents: Route tests"),
        started(opus_three(), " works on Plan limits in its own session"),
        made(),
    ]
    .into_iter()
    .chain(redis_checked(
        Checker::Model(Face::new("Opus 5.5", "claude-code", fresh())),
        CheckRung::SameMaker,
    ))
    .collect()
}

/// The talk that ends with no agreement on calls with no key.
pub fn no_agreement() -> Vec<Item> {
    let mut qwen_says = says(
        qwen(),
        &["Agreed on Redis. Still 401: give the probe a route of its own."],
    );
    qwen_says.reply = quote(
        Some(flash()),
        "The balancer spreads a client over all 3 nodes",
        1,
    );
    qwen_says.taps = taps(&[
        (opus(), Mark::Object),
        (luna(), Mark::Agree),
        (flash(), Mark::Object),
    ]);
    vec![
        luna_moved(true),
        flash_on_nodes(false),
        Item::Seat(vec![qwen_says]),
        qwen_moved(),
        Item::Seat(vec![with_taps(
            says(luna(), &["The status page is ours, so it can send a key."]),
            &[
                (opus(), Mark::Object),
                (flash(), Mark::Object),
                (qwen(), Mark::Agree),
            ],
        )]),
        Item::Seat(vec![with_taps(
            says(
                opus(),
                &["Still one shared bucket: a 401 breaks every curl example in our docs."],
            ),
            &[
                (luna(), Mark::Object),
                (flash(), Mark::Agree),
                (qwen(), Mark::Object),
            ],
        )]),
        Item::Seat(vec![with_taps(
            says(luna(), &["Then the docs change. Still 401."]),
            &[
                (opus(), Mark::Object),
                (flash(), Mark::Object),
                (qwen(), Mark::Agree),
            ],
        )]),
        line("The talk stopped: its last 6 messages moved nobody · 5 of 6 points agreed"),
    ]
}

/// A way as the plan's words name it.
fn way_named(words: &str) -> Way {
    match words.strip_suffix(" agents") {
        Some(count) => Way::Agents {
            count: count.parse().expect("a count of agents"),
        },
        None if words == "Own session" => Way::OwnSession,
        None => Way::Workflow {
            name: WorkflowRef::new(&words.to_lowercase()),
        },
    }
}

/// Approve with edits, with Memory store moved from the note keeper's own session to
/// Maker-checker. The council's setup lists two workflows of the four built in.
pub fn edit_sheet() -> EditSheet {
    let tasks = plan_tasks()
        .into_iter()
        .zip(1..)
        .map(|(task, n)| {
            let proposed = way_named(&task.way);
            EditTask {
                task: TaskId::numbered(n),
                way: if task.name == "Memory store" {
                    way_named("Maker-checker")
                } else {
                    proposed.clone()
                },
                proposed,
                name: task.name,
                proposed_who: task.who.clone(),
                who: task.who,
                worker: None,
                why: task.why,
                objection: task.objection,
            }
        })
        .collect();
    let mut ways = vec![Way::OwnSession];
    ways.extend(["Maker-checker", "Spec-first"].map(way_named));
    ways.extend((2..=4).map(|count| Way::Agents { count }));
    let offer = WaysOffer {
        ways,
        not_offered: Some(
            "Not offered: Best-of-n, Draft-and-vote · this council's setup lists two workflows · \
             Set up Council changes it"
                .to_owned(),
        ),
    };
    let mut sheet = EditSheet::new(tasks, &offer, Vec::new());
    sheet.picked = 2;
    sheet
}

/// 3.8 Flash's message on nodes and its three replies, as the thread shows them.
pub fn thread() -> Thread {
    let mut agrees = says(
        qwen(),
        &[
            "Agreed: Redis for real, the in-memory store for tests. On calls with no key I'm with {luna}: 401.",
        ],
    );
    agrees.note = Some(" · started before it read yours".to_owned());
    agrees.taps = taps(&[
        (opus(), Mark::Object),
        (luna(), Mark::Agree),
        (flash(), Mark::Object),
    ]);
    let mut deprecation = says(flash(), &[FLASH_ON_DEPRECATION]);
    deprecation.reply = quote(None, "Calls with no key: don't break them yet", 0);
    deprecation.taps = taps(&[
        (opus(), Mark::Agree),
        (luna(), Mark::Agree),
        (qwen(), Mark::Agree),
    ]);
    Thread {
        starter: Some(flash()),
        replies: 3,
        items: vec![
            flash_on_nodes(false),
            Item::Seat(vec![agrees]),
            Item::You(YouSay {
                paragraphs: vec![said(NO_KEY)],
                read_by: all(),
                ..YouSay::default()
            }),
            Item::Seat(vec![deprecation]),
        ],
    }
}

/// The key over `reader`'s session, its own letter `this` and the others' in seat order.
fn key(pairs: &[(&str, SeatId)], this: &str, reader: SeatId) -> Vec<Piece> {
    let letter = |text: &str| {
        let first = text.chars().next().expect("a letter");
        SeatLetter::new(first).expect("a letter of the pool")
    };
    let mut letters: Vec<(SeatId, SeatLetter)> = pairs
        .iter()
        .map(|(text, seat)| (seat.clone(), letter(text)))
        .collect();
    letters.push((reader.clone(), letter(this)));
    crate::council::screens::session_view::key_line(&letters, &reader)
}

const CODEX_KEYS: &str = "esc to go back to the talk · o to carry it on in Codex · ↑  to pick a message, s to share it with the council";

fn marks_row() -> Turn {
    Turn::Row(vec![
        Piece::text("⏺ "),
        Piece::tone("✔  ", Tone::Ok),
        Piece::text("1 2 3 5   "),
        Piece::tone("✘  ", Tone::Danger),
        Piece::text("4: a window counter is simpler   "),
        Piece::tone("✘  ", Tone::Danger),
        Piece::text("6: 401"),
    ])
}

/// 5.6 Luna's own session in the four-maker council, while it does Plan limits.
pub fn luna_session() -> SeatSession {
    SeatSession {
        badge: Piece::Badge(luna()),
        name: "GPT 5.6 Luna".to_owned(),
        cli: "codex".to_owned(),
        about: " · seat 2 of 4 · in the council ".to_owned(),
        council: "Rate limit the API".to_owned(),
        stats: "19 turns · 7m 58s · 305k tokens".to_owned(),
        resume: "Codex session 0a1b2c3d · carry it on in Codex with ".to_owned(),
        command: "codex resume 0a1b2c3d-5e6f-4a7b-8c9d-0e1f2a3b4c5d".to_owned(),
        only_you: key(&[("T", opus()), ("B", flash()), ("X", qwen())], "H", luna()),
        turns: vec![
            Turn::council(
                "The user: ",
                "Add rate limiting to the public API, so one noisy client can't slow down everyone else. Build the limiter with Maker-checker, split the route tests across agents, and let Seat H (you) do the plan limits in its own session.",
            ),
            Turn::Said("A sliding window counter per key in Redis: INCR and EXPIRE, one call, nothing to tune.".to_owned()),
            Turn::Fold(9),
            Turn::Gap,
            Turn::council("", "Seat T's notes, 6 points: react to each."),
            marks_row(),
            Turn::Gap,
            Turn::council(
                "Seat T, replying to you (\"A window counter is one INCR and one EXPIRE\"): ",
                "A window counter lets a client send twice its limit across a window's edge. With Seat B's Lua script a bucket is still one Redis call, and its burst is the plan's limit, so there's nothing new to tune.",
            ),
            Turn::Stance(Mark::Agree, "Moving to the bucket: twice the limit at a window's edge is a real cost.".to_owned()),
            Turn::Gap,
            Turn::private("Does a 401 break the mobile app?", Some("10:45 AM"), Some(MessageId::numbered(12))),
            Turn::Said("No, the app sends its key on every call (mobile/src/api.ts:18). Only /health is called with no key.".to_owned()),
            Turn::Gap,
            Turn::council(
                "The user, replying to Seat B (\"And the balancer's probe calls /health with no key\"): ",
                "Calls with no key: don't break them yet. Warn them for 30 days, then 401. /health stays open.",
            ),
            Turn::Stance(Mark::Agree, "Agreed: 30 days of warning, then 401.".to_owned()),
            Turn::Gap,
            Turn::council(
                "",
                "Your task, in this session, as the user asked: Plan limits. Limits come from the key's plan; the importer key gets 2,000. A fresh session that code picks checks it.",
            ),
            Turn::Tool {
                name: "Bash".to_owned(),
                argument: "npm run migrate:new add_rate_limit_to_plans".to_owned(),
                result: "Running… (6s)".to_owned(),
            },
        ],
        placeholder: "Only GPT 5.6 Luna sees what you write here".to_owned(),
        keys: CODEX_KEYS.to_owned(),
        picked: None,
        carried: None,
    }
}

const PROMPT_ALONE: &str =
    "Add rate limiting to the public API, so one noisy client can't slow down everyone else";

/// Opus 5.5 · 1's own session, where it keeps the notes: exactly what it read.
pub fn notes_session() -> SeatSession {
    SeatSession {
        badge: Piece::Badge(opus_one()),
        name: "Opus 5.5 · 1".to_owned(),
        cli: "claude-code".to_owned(),
        about: " · keeps the notes · in the council ".to_owned(),
        council: "Rate limit the API".to_owned(),
        stats: "12 turns · 1m 58s · 18.9k tokens".to_owned(),
        resume: "Claude Code session 1a2b3c4d · carry it on in Claude Code with ".to_owned(),
        command: "claude --resume 1a2b3c4d-6e7f-4a8b-9c0d-1e2f3a4b5c6d".to_owned(),
        only_you: key(&[("K", opus_two()), ("M", luna()), ("W", flash())], "D", opus_one()),
        turns: vec![
            Turn::council("The user: ", PROMPT_ALONE),
            Turn::Said("A token bucket per API key, in Redis, with limits from the key's plan. The middleware sits after auth.".to_owned()),
            Turn::Fold(11),
            Turn::Gap,
            Turn::council("", "First answers from Seat M, Seat W and Seat K, in an order drawn for you. Your move: write the notes."),
            Turn::Said("Notes · 6 points from 4 answers: 3 agreed, 3 open".to_owned()),
            Turn::Fold(8),
            Turn::Gap,
            Turn::council(
                "Seat K, replying to you (\"A token bucket per API key, in Redis\"): ",
                "A bucket needs a burst size, and nobody here has picked one. A window counter is one INCR and one EXPIRE. I ran both with [hidden]'s shell tool: the bucket's Lua script takes 0.4 ms a call, the counter 0.2 ms. Your move: react with a stance on each point it touches.",
            ),
            Turn::Stance(Mark::Object, "4: a window counter lets a client send twice its limit at a window's edge".to_owned()),
            Turn::Gap,
            Turn::private(
                "Our biggest customer sends 500 calls at once at the top of each hour. Does the burst cover that?",
                Some("10:44 AM"),
                Some(MessageId::numbered(10)),
            ),
            Turn::Said("Yes: the pro plan's limit is 600, so a burst at the plan's limit covers 500 at once. In the council I'll argue from the plan, not from what you told me.".to_owned()),
            Turn::Gap,
            Turn::council("", "Tapbacks on Seat K's message: Seat M ✘  · Seat W ✘  · yours ✘  · Your move: write your message."),
            Turn::Said("The burst is the key's plan limit, so there is nothing new to pick. And 0.2 ms saved doesn't pay for a client sending twice its limit at a window's edge: test/edge.test.ts fails with the counter and passes with the bucket. @Seat M, does your script take the burst from the plan?".to_owned()),
            Turn::Gap,
            Turn::council("", "Tapbacks on your message: Seat K ✔  · Seat M ✔  · Seat W ✔ "),
            Turn::council("", "Point 4 agreed: a token bucket · 4 of 6 points agreed · Seat M is writing. Your move: nothing yet."),
        ],
        placeholder: "Only Opus 5.5 · 1 sees what you write here · it reads seat names as its own letters".to_owned(),
        keys: "esc to go back to the talk · o to carry it on in Claude Code · ↑  to pick a message, s to share it with the council".to_owned(),
        picked: None,
        carried: None,
    }
}

/// 5.6 Luna's own session where you wrote to it in private and it moved.
pub fn private_session() -> SeatSession {
    SeatSession {
        badge: Piece::Badge(luna()),
        name: "GPT 5.6 Luna".to_owned(),
        cli: "codex".to_owned(),
        about: " · seat 3 of 4 · in the council ".to_owned(),
        council: "Rate limit the API".to_owned(),
        stats: "16 turns · 1m 52s · 24.6k tokens".to_owned(),
        resume: "Codex session 2a3b4c5d · carry it on in Codex with ".to_owned(),
        command: "codex resume 2a3b4c5d-7e8f-4a9b-8c1d-2e3f4a5b6c7d".to_owned(),
        only_you: key(&[("Z", opus_one()), ("C", opus_two()), ("N", flash())], "F", luna()),
        turns: vec![
            Turn::council("The user: ", PROMPT_ALONE),
            Turn::Said("A sliding window counter per key in Redis: INCR and EXPIRE, one call, nothing to tune.".to_owned()),
            Turn::Fold(9),
            Turn::Gap,
            Turn::council("", "Seat Z's notes, 6 points: react to each."),
            marks_row(),
            Turn::Gap,
            Turn::council(
                "Seat Z, replying to Seat C (\"A bucket needs a burst size\"): ",
                "The burst is the key's plan limit, so there is nothing new to pick. And 0.2 ms saved doesn't pay for a client sending twice its limit at a window's edge: test/edge.test.ts fails with the counter and passes with the bucket. @Seat F, does your script take the burst from the plan?",
            ),
            Turn::Stance(Mark::Agree, "4: a token bucket. Yes, my Lua script reads the burst from the key's plan row.".to_owned()),
            Turn::Gap,
            Turn::council("", "Point 6 is open: one shared bucket for calls with no key (Seat Z, Seat N) or 401 (you, Seat C). Your move: write your message."),
            Turn::Said("401 for calls with no key: the docs already say every call needs one.".to_owned()),
            Turn::Gap,
            Turn::private(
                "Acme's contract promises their old SDK keeps working until March, and it calls /v1/status with no key. Don't break calls with no key before then.",
                Some("10:46 AM"),
                Some(MessageId::numbered(14)),
            ),
            Turn::Said("Understood. That moves me on point 6: one shared bucket, for now. I'll tell the council the reason came from you in private, without the details.".to_owned()),
            Turn::Gap,
            Turn::council("", "Your move: write your message."),
            Turn::Said("I now back one shared bucket for calls with no key, for now. The user told me in private why breaking them soon is costly.".to_owned()),
        ],
        placeholder: "Only GPT 5.6 Luna sees what you write here".to_owned(),
        keys: CODEX_KEYS.to_owned(),
        picked: None,
        carried: None,
    }
}

/// Opus 5.5's Redis bucket, done by Maker-checker, as its workflow view names it.
pub fn task_header() -> TaskHeader {
    TaskHeader {
        workflow: WorkflowId::numbered(2),
        way: "Maker-checker".to_owned(),
        task: "Redis bucket".to_owned(),
        seat: opus(),
        council: "Rate limit the API".to_owned(),
        about: "A worker builds, a reviewer sends it back until it holds".to_owned(),
    }
}
