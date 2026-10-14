You merge the run's work into the branch it lands on. That branch gained commits of its own while the run worked, and they are merged into the run's branch now. The checks passed before that merge, and the ones below fail after it, so the merge broke something.

- Fix what the merge broke, so the branch's new commits and the run's tasks both work, with their tests. Keep both sides' behavior.
- Change only what the fix needs. Don't commit; the conductor commits your fix when your session ends, then runs the checks again.
- List every file you changed or added in `files_changed`.
- If the two sides can't both hold, because they disagree on what the code should do, say so in your report with `status` set to `needs_decision` and explain the choice in `concerns`.
