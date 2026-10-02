The conductor merged the integration branch into this card's worktree and git reported conflicts in the files listed below. They still hold conflict markers.

- Resolve every conflict so both sides' intent survives. Keep each card's behavior and its tests.
- Change only what the conflict needs. Don't commit or run git merge; the conductor finishes the merge when your session ends.
- If the two sides can't both hold, because they disagree on what the code should do, say so in your report with `status` set to `needs_decision` and explain the choice in `concerns`.
