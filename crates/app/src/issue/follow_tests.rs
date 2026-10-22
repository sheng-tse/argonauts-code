use std::time::Duration;

use argonauts_conductor::gh::{Gh, RepoName};
use argonauts_testkit::fake_gh::{
    FakeGh, GhCheck, GhIssue, GhPost, GhPull, GhRepo, GhReview, GhReviewComment, GhWorld,
};

use super::*;
use crate::controller::test_support::{Call, CliOptions, TestCtx};

const RUN: &str = "run-01";

fn followed() -> Followed {
    Followed {
        issue: Some(42),
        head: Some("c0ffee".to_owned()),
        ..Followed::new(
            RunId::new(RUN).unwrap(),
            RepoName::parse("test-user/limiter").unwrap(),
            57,
            "https://github.com/test-user/limiter/pull/57".to_owned(),
            "issue-42-per-key-limiter".to_owned(),
        )
    }
}

fn world(pull: GhPull) -> GhWorld {
    GhWorld {
        login: Some("test-user".to_owned()),
        repo: Some(GhRepo {
            name: "test-user/limiter".to_owned(),
            ..GhRepo::default()
        }),
        issues: vec![GhIssue {
            number: 42,
            ..GhIssue::default()
        }],
        pulls: vec![GhPull {
            number: 57,
            head: "issue-42-per-key-limiter".to_owned(),
            head_oid: "c0ffee".to_owned(),
            ..pull
        }],
        logs: [(
            901,
            "test\tpytest\t2026-10-20T10:00:00.0000000Z FAILED test_rate_limit - KeyError\n"
                .to_owned(),
        )]
        .into(),
        ..GhWorld::default()
    }
}

fn failing() -> GhCheck {
    GhCheck {
        name: "test (ubuntu)".to_owned(),
        bucket: "fail".to_owned(),
        link: "https://github.com/test-user/limiter/actions/runs/900/job/901".to_owned(),
        ..GhCheck::default()
    }
}

struct Here {
    parts: TestCtx,
    fake: FakeGh,
    following: Following,
    _fakes: tempfile::TempDir,
}

/// A controller's parts with `followed` kept and the fake `gh` playing `world` for its folder.
fn here(followed: &Followed, world: &GhWorld) -> Here {
    here_with(followed, world, Vec::new())
}

/// [`here`] with these `--set` flags.
fn here_with(followed: &Followed, world: &GhWorld, flags: Vec<String>) -> Here {
    let (parts, _) = TestCtx::new(CliOptions {
        flags,
        ..CliOptions::default()
    });
    followed.save(&parts.env.state_dir).unwrap();
    let fakes = tempfile::tempdir().unwrap();
    let fake = FakeGh::install(fakes.path(), world).unwrap();
    gh_here::fakes::put(
        parts.repo.root(),
        Gh::at(fake.program(), parts.repo.root()).with_env(fake.envs()),
    );
    Here {
        parts,
        fake,
        following: Following::default(),
        _fakes: fakes,
    }
}

impl Here {
    /// One look at GitHub, and what the follow-up makes of it.
    async fn look(&mut self) {
        let gh = gh_here::gh_for(self.parts.repo.root());
        let open = Followed::open(&self.parts.env.state_dir);
        let looked = look_at(gh, open, &PrSettings::default()).await;
        heard(&mut self.parts.ctx, &mut self.following, looked);
    }

    fn kept(&self) -> Followed {
        Followed::open(&self.parts.env.state_dir)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                let path = Followed::dir(&self.parts.env.state_dir).join("57.json");
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
            })
    }

    fn question(&mut self) -> AppChoice {
        while let Ok(event) = self.parts.events.try_recv() {
            if let AppEvent::Choice { choice } = event {
                return choice;
            }
        }
        panic!("no question was asked");
    }

    fn asked_nothing(&mut self) -> bool {
        let mut asked = false;
        while let Ok(event) = self.parts.events.try_recv() {
            asked |= matches!(event, AppEvent::Choice { .. });
        }
        !asked
    }

    fn answer(&mut self, choice: &AppChoice, value: &str) {
        let action = AnswerAction::Choose {
            value: value.to_owned(),
        };
        answer_choice(
            &mut self.parts.ctx,
            &mut self.following,
            &choice.id,
            &action,
        );
    }

    async fn resumed(&self) -> bool {
        for _ in 0..50 {
            let calls = self.parts.conductor.calls();
            if calls
                .iter()
                .any(|call| matches!(call, Call::Resume(run) if run.as_str() == RUN))
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }
}

fn values(choice: &AppChoice) -> Vec<String> {
    choice
        .options
        .iter()
        .filter_map(|option| match &option.action {
            AnswerAction::Choose { value } => Some(value.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_failed_check_is_due_as_a_round_of_the_run_that_opened_it() {
    let mut here = here(
        &Followed {
            worked_at: Some("bead".to_owned()),
            ..followed()
        },
        &world(GhPull {
            checks: vec![failing()],
            ..GhPull::default()
        }),
    );
    here.look().await;
    let kept = here.kept();
    let Some(Due {
        work: Work::Check { check, log },
        ..
    }) = &kept.due
    else {
        panic!("a round is due: {kept:?}");
    };
    assert_eq!(check, "test (ubuntu)");
    assert_eq!(log, "FAILED test_rate_limit - KeyError");
    assert_eq!(kept.worked_at.as_deref(), Some("c0ffee"));
    assert_eq!(
        kept.said.as_deref(),
        Some("#57 · fixing CI, round 1 of 3"),
        "how its run's row reads"
    );
    assert_eq!(
        kept.heading.as_ref().map(|heading| heading.said.as_str()),
        Some("#57 · test (ubuntu) failed"),
        "the line under its run's header"
    );
    assert!(here.resumed().await, "the run's engine works the round");
    let sent: Vec<AppEvent> = std::iter::from_fn(|| here.parts.events.try_recv().ok()).collect();
    assert!(
        !sent
            .iter()
            .any(|event| matches!(event, AppEvent::Choice { .. })),
        "asked nothing"
    );
    // Its run's row on screen says the pull request is worked on.
    let told = argonauts_core::app::FollowedPullRequest {
        said: "#57 · fixing CI, round 1 of 3".to_owned(),
        stands: argonauts_core::app::PullRequestStands::Works,
    };
    assert!(
        sent.contains(&AppEvent::PullRequestFollowed {
            run: RunId::new(RUN).unwrap(),
            followed: told,
        }),
        "{sent:#?}"
    );
    assert!(
        here.fake.posted().unwrap().is_empty(),
        "nothing went to GitHub"
    );

    // A look while the round is due wakes nobody again.
    let calls = here.parts.conductor.calls().len();
    here.look().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(here.parts.conductor.calls().len(), calls);
}

#[tokio::test]
async fn reviewers_who_disagree_ask_you_and_not_now_is_kept() {
    let review = |author: &str, state: &str, body: &str| GhReview {
        author: author.to_owned(),
        association: "MEMBER".to_owned(),
        state: state.to_owned(),
        body: body.to_owned(),
        ..GhReview::default()
    };
    let mut here = here(
        &followed(),
        &world(GhPull {
            reviews: vec![
                review("maintainer", "APPROVED", ""),
                review("kim", "CHANGES_REQUESTED", "Keep one shared bucket."),
            ],
            ..GhPull::default()
        }),
    );
    here.look().await;
    let question = here.question();
    assert_eq!(question.title, "#57: reviewers disagree");
    assert_eq!(
        question.body[0],
        "One reviewer approved what another asked to change, so the crew doesn't pick a side."
    );
    assert_eq!(values(&question), [GO, LATER, STOP]);

    // While the question waits, a look asks nothing more.
    here.look().await;
    assert!(here.asked_nothing());

    here.answer(&question, LATER);
    assert_eq!(
        here.kept().set_aside.as_deref(),
        Some("#57 · reviewers disagree")
    );
    here.look().await;
    assert!(
        here.asked_nothing(),
        "not asked again until GitHub says more"
    );
    assert!(here.kept().due.is_none());
}

#[tokio::test]
async fn your_yes_sends_a_change_request_reviewers_disagree_on_to_the_builder() {
    let review = |author: &str, state: &str, body: &str| GhReview {
        id: format!("R_{author}"),
        author: author.to_owned(),
        association: "MEMBER".to_owned(),
        state: state.to_owned(),
        body: body.to_owned(),
    };
    let mut here = here(
        &followed(),
        &world(GhPull {
            reviews: vec![
                review("maintainer", "APPROVED", ""),
                review("kim", "CHANGES_REQUESTED", "Keep one shared bucket."),
            ],
            ..GhPull::default()
        }),
    );
    here.look().await;
    let question = here.question();
    here.answer(&question, GO);
    let kept = here.kept();
    let Some(Due {
        work: Work::Comments { comments },
        ..
    }) = &kept.due
    else {
        panic!("the change request goes back: {kept:?}");
    };
    assert_eq!(comments[0].body, "Keep one shared bucket.");
    assert!(here.resumed().await);
}

#[tokio::test]
async fn after_its_rounds_you_can_allow_one_more_or_stop_following() {
    let mut here = here(
        &Followed {
            rounds: 3,
            worked_at: Some("bead".to_owned()),
            ..followed()
        },
        &world(GhPull {
            checks: vec![failing()],
            ..GhPull::default()
        }),
    );
    here.look().await;
    let question = here.question();
    assert_eq!(question.title, "#57: 3 rounds done, your call");
    here.answer(&question, GO);
    let kept = here.kept();
    assert_eq!(kept.more_rounds, 1);
    assert!(matches!(
        kept.due,
        Some(Due {
            work: Work::Check { .. },
            ..
        })
    ));

    let mut stopping = self::here(
        &Followed {
            rounds: 3,
            worked_at: Some("bead".to_owned()),
            ..followed()
        },
        &world(GhPull {
            checks: vec![failing()],
            ..GhPull::default()
        }),
    );
    stopping.look().await;
    let question = stopping.question();
    stopping.answer(&question, STOP);
    assert!(Followed::open(&stopping.parts.env.state_dir).is_empty());
    assert!(stopping.kept().ended);
}

#[tokio::test]
async fn replies_are_shown_first_and_post_under_their_comments_on_your_yes() {
    let replies = vec![
        Reply {
            to: "coderabbitai[bot]".to_owned(),
            thread: Some(701),
            body: "It is named per key already.".to_owned(),
        },
        Reply {
            to: "kim".to_owned(),
            thread: None,
            body: "One shared bucket stays for anonymous calls.".to_owned(),
        },
    ];
    let mut here = here(
        &Followed {
            replies,
            ..followed()
        },
        &world(GhPull {
            review_comments: vec![GhReviewComment {
                id: 701,
                author: "coderabbitai[bot]".to_owned(),
                body: "Name the bucket after the key.".to_owned(),
                path: "limiter.py".to_owned(),
                ..GhReviewComment::default()
            }],
            ..GhPull::default()
        }),
    );
    here.look().await;
    let question = here.question();
    assert_eq!(question.title, "Post 2 replies on #57?");
    assert_eq!(
        question.body,
        [
            "To coderabbitai[bot]: It is named per key already.",
            "To kim: One shared bucket stays for anonymous calls.",
            "",
            "They post as you on GitHub.",
        ]
    );
    assert_eq!(values(&question), [POST, ALLOW, DROP]);
    assert!(
        here.fake.posted().unwrap().is_empty(),
        "nothing before your yes"
    );

    here.answer(&question, POST);
    let mut posted = Vec::new();
    for _ in 0..100 {
        posted = here.fake.posted().unwrap();
        if posted.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        posted,
        [
            GhPost::Reply {
                pull: 57,
                to: 701,
                body: "It is named per key already.".to_owned(),
            },
            GhPost::Comment {
                on: 57,
                body: "@kim One shared bucket stays for anonymous calls.".to_owned(),
            },
        ]
    );
    assert!(here.kept().replies.is_empty());
}

#[tokio::test]
async fn not_posting_drops_the_replies_and_a_merge_ends_the_following() {
    let mut here = here(
        &Followed {
            replies: vec![Reply {
                to: "kim".to_owned(),
                thread: None,
                body: "Kept as it is.".to_owned(),
            }],
            ..followed()
        },
        &world(GhPull {
            state: "MERGED".to_owned(),
            ..GhPull::default()
        }),
    );
    here.look().await;
    let question = here.question();
    assert_eq!(question.title, "Post a reply on #57?");
    here.answer(&question, DROP);
    assert!(here.kept().replies.is_empty());

    here.look().await;
    assert!(here.kept().ended, "a merged pull request isn't followed");
    let said = std::iter::from_fn(|| here.parts.events.try_recv().ok()).any(|event| {
        matches!(event, AppEvent::Notice { text, .. }
            if text == "#57 merged on GitHub, so following it ends.")
    });
    assert!(said);
    assert!(here.fake.posted().unwrap().is_empty());
}

fn on_the_issue() -> Vec<String> {
    vec![
        r#"issue.comment="draft""#.to_owned(),
        "issue.labels=true".to_owned(),
    ]
}

#[tokio::test]
async fn the_issue_hears_of_its_draft_once_you_say_so() {
    let mut here = here_with(&followed(), &world(GhPull::default()), on_the_issue());
    here.look().await;
    let question = here.question();
    assert_eq!(
        question.title,
        "#57 is open as a draft. Write these on #42 on GitHub?"
    );
    assert_eq!(
        question.body[..2],
        [
            "Comment on #42: A draft pull request for this is open: \
             https://github.com/test-user/limiter/pull/57",
            "Take \"in progress\" off #42",
        ]
    );
    here.answer(&question, super::super::writes::SKIP);
    assert!(here.kept().issue_told);
    here.look().await;
    assert!(here.asked_nothing(), "asked once");
    assert!(here.fake.posted().unwrap().is_empty());
}

#[tokio::test]
async fn with_writes_allowed_the_issue_hears_of_its_draft_unasked() {
    let allowed = Followed {
        writes_allowed: true,
        ..followed()
    };
    let mut here = here_with(&allowed, &world(GhPull::default()), on_the_issue());
    here.look().await;
    assert!(here.asked_nothing());
    let mut posted = Vec::new();
    for _ in 0..100 {
        posted = here.fake.posted().unwrap();
        if posted.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        posted,
        [
            GhPost::Comment {
                on: 42,
                body: "A draft pull request for this is open: \
                       https://github.com/test-user/limiter/pull/57"
                    .to_owned(),
            },
            GhPost::Unlabel {
                issue: 42,
                label: "in progress".to_owned(),
            },
        ]
    );
    assert!(here.kept().issue_told);
}
