## What and why

<!-- What this changes and why. Link the issue it closes, for example "Fixes #123". -->

## How it was tested

<!-- The commands you ran and the tests you added. For a screen change, a before and after screenshot. -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `CI=true cargo test --workspace` pass
- [ ] New behavior has a test, and changed snapshots were reviewed with `cargo insta review`
- [ ] No keys, tokens or personal details in the code, recordings or logs
- [ ] The title follows Conventional Commits, for example `fix(codex): read the new usage line`
