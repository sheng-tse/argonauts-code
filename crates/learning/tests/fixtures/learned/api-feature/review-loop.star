# Blind drafts and a judge, then each task built in a loop with checks and two blind reviewers.
REVIEWERS = 2

def reviewers(n, blind = True):
    return [
        review(name = "review-%d" % i, role = "reviewer", blind = blind, different_provider_from = "author")
        for i in range(1, n + 1)
    ]

def task_steps():
    steps = [build(name = "build", role = "worker"), checks(name = "checks")]
    steps.extend(reviewers(REVIEWERS))
    return steps

phases = [
    phase("Understand", "understand", [read(role = "reader", count = 3, blind = True)]),
    phase("Plan", "plan", [
        draft(name = "drafts", role = "draft", count = 2, blind = True),
        judge(name = "pick", role = "judge", input = "drafts"),
    ]),
]

task = [loop_until(steps = task_steps(), until = "reviewers_approve", max = 3)]
if REVIEWERS > 1:
    task.append(synthesize(name = "settle", role = "judge", when = "reviewers_disagree"))

phases.append(phase("Build", "do", [for_each(input = "plan", steps = task)]))
phases.append(phase(name = "Ship", standard = "decide", steps = [merge()]))

workflow(
    name = "review-loop",
    description = "Blind drafts, then a checked build loop with two blind reviewers",
    phases = phases,
)
