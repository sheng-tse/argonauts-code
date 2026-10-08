Code checked the brief you just wrote and can't use it yet. Each line of `errors` in your input is one problem with it. Fix every one, keep the rest of the brief as it was, and answer with the whole brief again as one JSON object in the same schema. You've read the repository already, so read it again only if a problem needs it. This session is still read-only. If your first input had a `current` workflow, you were changing a running one: the run keeps its name, description, `based_on`, and done-when list whatever you write, so fix the `workflow`.

What the brief needs:
- `name`: two to four words about the task, lowercase, joined with `-`, at most 32 characters, such as `fix-slugify-median`. Never an example's name.
- `description`: the task in one plain line of at most 72 characters.
- `based_on`: the example you started from, by name, or null when you composed from the step kinds.
- `workflow`: null to run `based_on` as written, else the whole workflow file as YAML text: one to six phases, each with a `title` of one or two words in Title Case and a one-line `detail`, and a `label` on every step.
- `done_when`: one to eight criteria, each with an `id` (`G1`, `G2`, ...), a `short` name of a few words, one sentence of `text` a check can prove, and its `check`: `"review"` for a reading of the diff, `{ "test": "<test name>" }`, or `{ "command": "<command>" }`.
- `plan`: empty, or the one task a build from the goal makes, with a one-token `item` and a `task`.
- `questions`: at most three.
- `first_attempt`: null, unless the run is a goal.
