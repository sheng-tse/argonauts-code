# Blind drafts and a judge, then each card built in a loop with gates and two blind reviewers.
REVIEWERS = 2

def reviewers(n, blind = True):
    return [
        review(name = "review-%d" % i, role = "reviewer", blind = blind, different_family_from = "author")
        for i in range(1, n + 1)
    ]

def card_steps():
    steps = [build(name = "build", role = "owner"), gate(name = "gates")]
    steps.extend(reviewers(REVIEWERS))
    return steps

phases = [
    phase("Understand", "understand", [restate(role = "restater", count = 3, blind = True)]),
    phase("Plan", "plan", [
        draft(name = "drafts", role = "drafter", count = 2, blind = True),
        judge(name = "pick", role = "judge", input = "drafts"),
    ]),
]

card = [loop_until(steps = card_steps(), until = "reviewers_approve", max = 3)]
if REVIEWERS > 1:
    card.append(synthesize(name = "settle", role = "adjudicator", when = "reviewers_disagree"))

phases.append(phase("Build", "do", [for_each(input = "plan", steps = card)]))
phases.append(phase(name = "Ship", standard = "decide", steps = [merge()]))

workflow(
    name = "review-loop",
    description = "Blind drafts, then a gated build loop with two blind reviewers",
    phases = phases,
)
