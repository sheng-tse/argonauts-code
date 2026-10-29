<!-- The title becomes the squash commit: a Conventional Commit such as `fix(codex): read the new usage line`. One change per pull request. -->

## Summary

<!-- What this changes and why, in a few sentences. Say what a user sees differently, if anything. -->

## Related issues

<!-- "Fixes #123" closes the issue when this merges. For anything bigger than a small fix, link the issue where the approach was agreed. -->

## How it was tested

<!-- The commands you ran and what they printed, and the tests you added or changed. For a screen change, add a before and after screenshot. For a change to how a CLI is read, name the recordings you replayed or re-recorded. -->

## Checklist

### Checks

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `CI=true cargo test --workspace` passes

### Tests

- [ ] New or changed behavior has a test
- [ ] A changed `.snap` file or golden picture under `crates/*/tests/golden/` changed only to match a drawing in `approved/`, never just to make a test pass
- [ ] No test calls a model or touches your own `~/.argonauts`

### Recordings and privacy (skip if no recording changed)

- [ ] Recordings were made with `crates/conductor/scripts/rerecord-fixtures.sh` or by hand as CONTRIBUTING.md says, with the CLI version in the `.meta.json` beside them
- [ ] Nothing about you is in them: account plan, email, home path, account or organization ids, your installed skills, plugins or MCP servers
- [ ] The `retired_names` test passes

### Docs

- [ ] User-facing changes are in README.md or the docs, and in CHANGELOG.md under the next version
- [ ] A new or changed default is in the frozen defaults, `crates/core/tests/fixtures/settings-defaults.toml`
- [ ] Markdown keeps each paragraph and list item on one line

### Safety (skip if none apply)

- [ ] This touches the sandbox, approvals, the never-list, writes outside the project or merging, and the summary says how it behaves in each mode, with a test for each

## Notes for reviewers

<!-- Optional: what to look at first, what you're unsure of, or what you left out on purpose. -->
