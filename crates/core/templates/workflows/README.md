# Workflows

A workflow says how the crew works on a request. Each file here is one workflow, and its name is a command: `/spec-first add due dates` runs one request with `spec-first.toml`.

- `/workflow` lists every workflow, the built-in ones and yours.
- `/workflow new <name>` writes a template here. Add `from council` to start from a built-in one.
- `/workflow try <name> <request>` shows what would run, and offers to run it or edit it. Nothing runs until you say so.
- `/workflow <name>` makes it the workflow in use.
- `/workflow edit <name>` opens its page: the steps in order, each with its keys. A change is checked and saved to the file with your comments kept; one that would stop the steps from running is refused and says why. `e` opens the file in your editor (`$VISUAL` or `$EDITOR`), and a save from any editor is checked on the page too. A script has no keys to list, so it goes straight to your editor.
- `/workflows` (Ctrl+T with no run on screen) is the library of them all: Enter shows one, `c` copies a built-in one out to change, `d` compares your copy with it, and `e` opens your file's page.
- `s` in a run's workflow view keeps the script its orchestrator wrote as a workflow of yours, named after the run, so `/<that name>` runs it again.

A file in this folder wins over one of the same name in `~/.argonauts/workflows/`, and that wins over a built-in one.

## A file of steps

```toml
name  = "spec-first"
about = "Write the spec first, then build to it."
adapt = false            # false: the steps run exactly as written
                         # true: the orchestrator writes the steps for each request and keeps
                         #       who plans, builds, reviews and signs off as below
# goal = "every test passes"   # go round the whole file until this holds

[[step]]
name = "Plan"
role = "orchestrator"
does = "Split the request into tasks: {request}"
```

Steps run from the top. Each one is a phase in the workflow view.

With `adapt = false` the steps run exactly as written. With `adapt = true`, which the built-in workflows have, the orchestrator writes the steps for each request as a script, and keeps to the roles the file names. Its script is rehearsed against placeholder answers before any agent starts. One that can't be used is sent back to it once, and after that the file's own steps run as written. In supervised mode the steps wait for you first: approve them, send them back with a note, run the request as Captain, or abandon it.

`workflows.adapt_with = "phases"` runs the workflow's phases in place of the orchestrator's script. They are the `[[phases]]` tables at the end of its file, as the built-in workflows have them. A workflow with no phases of its own, such as a file of steps you wrote, runs the crew's fixed phases: blind drafts, a map of where they disagree, critique, then one plan merged or judged. Council runs its talk in their place: every seat answers alone, then the seats talk until every seat agrees on every point. Phases saved from a run's workflow view sit here as `<name>.yaml`, a file of phases alone, and `/workflow <name>` picks them like any other workflow.

| Key | What it does |
|---|---|
| `name` | What the step is called. Other steps point at it by this name. |
| `role` | Who takes it: `orchestrator`, `worker`, `reviewer`, `judge` or `main`. The `[crew]` settings say which model fills each role. |
| `does` | What its agents are asked. `{request}` is your request and `{task}` the task's title. Without it the role's usual ask is used. |
| `for_each = "task"` | One agent per task, side by side, each writer in its own worktree. The step before the first of these lists the tasks. Worker and reviewer steps after it stay per task until a judge or the orchestrator looks at the whole. |
| `count` | How many agents: a number, or `"all"` for every model the role lists. They come from the front of the role's list. |
| `independent` | Its agents don't see what the steps before them made. |
| `not_the_author` | Each task goes to an agent that didn't build it. |
| `send_back_to` | The step the work returns to when this one doesn't accept it. A step written below the one that sends to it runs only then. A step that gives no answer at all, because its session failed or its agent is out of plan usage, ends the task unaccepted instead of sending it back. |
| `then` | The step that runs next, in place of the one below. |
| `until` | A goal on this step: its agent keeps going until this is true. |
| `kept_by` | Who keeps an `until`: `cli` for the CLI's own goal where it has one, or `argonauts`. |
| `max_rounds` | The most times the step runs for one task; on a step with `until`, the most tries its goal gets. A loop that sends work back with no `max_rounds` in it stops after two send-backs. A `then` that goes back up needs a step with `max_rounds` in its loop, unless the `then` step runs only when work is sent back to it. |
| `model` | A model of its own, such as `codex/gpt-5.6`, in place of the role's. |
| `own_workflow` | Its workers may each run a workflow of their own, where their CLI has one. On Claude Code that is Claude Code's own workflow, and it shows nested under the worker. |
| `keep` | When the orchestrator writes the steps for a request, it carries this step out with its role, its access and as many agents as the step says, under a phase name of its own. A script that leaves it out is sent back before any agent starts. |

A file may also set settings below its steps, such as `[review]` keys. They apply while the workflow is in use. Its phases, when it has them, come last.

## More than one

A script may run another workflow from this folder as a step, with `workflow(name, args)`. Its agents show grouped under that workflow's name, and several may run side by side. One that was started this way can't start another, and `workflows.max_depth = 1` keeps every script from starting any.

A worker runs a workflow of its own only when you allow it: for one step with `own_workflow = true`, or for every agent of the crew with `crew.worker_workflows = true`. Otherwise its CLI starts none, nor any agent in the background, since Argonauts Code already runs the crew and one inside a worker can't be watched or merged. When allowed, its CLI fans out as it sees fit, the worker's session stays open until what it started reports, and what it starts shows nested under that worker. A CLI without workflows of its own works as it always does.

## How big, how often, and who runs it

- **Size.** Say it in the request, such as "use a small workflow, 5 agents max", and the orchestrator keeps to it; the workflow's row says what you asked. Otherwise `workflow_size` (small, medium, large, or none) is the guideline. A file that fixes its own steps keeps its own counts.
- **Ultracode**, the switch in `/effort` or the word `ultracode` typed with one request, changes how often a workflow is used, never what one is. With a crew workflow, small changes go to it too. In Captain, Claude Code runs its own, and on Codex or agy one runs with every agent on that same model.
- **Who runs it** follows the crew. With the orchestrator and every worker on Claude Code, it is Claude Code's own workflow: Claude Code plans it, its subagents do the work, and the reviewers and the judge are asked on their own models through `argonauts_ask_reviewer` and `argonauts_ask_judge`. In the workflow view such a step is the model that answered: "Astra · asked through Argonauts Code" while it reads, then "accepted" or "asked for changes". A final change that didn't go through them is reviewed before it merges. Otherwise Argonauts Code's runner runs the steps.
- **Roles you leave out follow the ones you set:** workers follow the orchestrator, and the judge follows the reviewers, whichever file sets them.
- **Your words set the crew for one request**, such as "all the other agents need to be Opus, and the reviewer will be Astra". The crew understood is shown first, each role with where it came from. It is for that request alone; words that say to keep it from now on save it where your changes go. "All the other agents" fills every role no other word names, except the reviewers, who stay on another provider unless you name them.
- **The main session can ask a role too**, when you ask it to ("have the reviewers look at this"): the reviewers or the judge read the change in your folder on their own model, and their answer comes back to it. That is their opinion, and nothing waits on it. What they spend counts in `/usage` with the conversation's turns.

## A script

For full control a workflow can be a script in the format Claude Code writes its own, saved here as `<name>.js`. A workflow you saved in Claude Code runs as it is.

```js
export const meta = {
  name: 'review-until-clean',
  description: 'Build, then review until the reviewer accepts',
  phases: [{ name: 'Build' }, { name: 'Review' }],
}

let findings = []
for (let round = 1; round <= 5; round++) {
  await agent(`Build: ${args.request}\n${findings.join('\n')}`, { role: 'worker', phase: 'Build' })
  const verdict = await agent('Review the change.', { role: 'reviewer', phase: 'Review', schema: VERDICT })
  if (verdict && verdict.accept) break
  findings = verdict ? verdict.findings : []
}
```

What a script may use, besides plain JavaScript:

| | |
|---|---|
| `agent(prompt, opts)` | Runs one agent and returns its answer: its final text, or the object `opts.schema` asks for. Null when it fails. `opts.role` names who takes it (`worker` unless said), `opts.label` its row, `opts.phase` its phase, `opts.model` a model of its own, `opts.task` the task it works on, `opts.isolation: 'worktree'` gives it a worktree of its own, and `opts.ownWorkflow: true` lets a worker run a workflow of its own. |
| `parallel(tasks)` | Runs a list of functions at once and waits for all of them. One that throws is a null. |
| `pipeline(items, ...stages)` | Sends each item through every stage by itself. |
| `phase(name)` | Starts a phase. Agents after it are grouped under it. |
| `log(message)` | A line above the phases. |
| `check(condition)` | Whether a condition holds yet: `{ holds, failed }`. |
| `workflow(name, args)` | Runs another workflow from this folder as one step. A file of steps takes `args.request`. |
| `args` | What the run was started with. `args.request` is your request. |
| `limit` | `total`, `spent()` and `remaining()` in tokens, when the run has a token target. |

A script has no files, no network, no clock and no random numbers: `Date.now()`, `new Date()` and `Math.random()` throw, so a run that stops can pick up where it was. Its memory and the time it runs between agents are capped. At most 16 agents work at once and 1,000 start in one run. `meta` must be the first statement and a plain literal.
