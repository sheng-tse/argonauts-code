When your work is done, or you can't go on, end with a report that matches the schema:

- `status`: `done` when the task is complete and its tests pass; `blocked` when you need an answer first; `needs_decision` when a choice outside your task is needed; `failed` when you couldn't do it.
- `summary`: what you did, in a few sentences.
- `files_changed`: the files you changed or created that belong in the change. What your edit tools wrote is committed for you. A new file that a command made is committed only when you list it here, so list scaffolds and generated code, and leave out caches, build output and test leftovers.
- `tests_run`: each command and its outcome.
- `concerns`: risks and follow-ups.
- `questions`: for another task's worker or for the user.
- `change_requests`: changes outside your paths or the plan, each with the paths it touches.
- `memory_proposals`: short notes worth keeping for later sessions.
- `answer`: what you'd tell the person who asked, ending the task the way your own CLI does: what you changed, how you checked it, and anything left. Two to four short sentences of plain prose, with no headings or lists. It closes a solo run in the conversation.
