#!/bin/sh
# Writes THIRD_PARTY_NOTICES.md: cargo-about's list of the licenses of every crate compiled into the argonauts
# binary (with the hand-written parts in about.hbs), then the licenses of the syntax definitions two-face bundles.
# Run it after any dependency change and commit the result.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
notices="$root/packaging/notices"
out="$root/THIRD_PARTY_NOTICES.md"

# The syntaxes tool has to read the same two-face the program is built with.
locked=$(awk '$0 == "name = \"two-face\"" { getline; gsub(/version = |"/, ""); print; exit }' "$root/Cargo.lock")
pinned=$(sed -n 's/^two-face = { version = "=\([^"]*\)".*/\1/p' "$notices/syntaxes/Cargo.toml")
case "$locked" in
    "$pinned" | "$pinned"+*) ;;
    *)
        echo "two-face is $locked in Cargo.lock but $pinned in packaging/notices/syntaxes/Cargo.toml: update the pin" >&2
        exit 1
        ;;
esac

crates=$(mktemp)
syntaxes=$(mktemp)
trap 'rm -f "$crates" "$syntaxes"' EXIT

cargo about generate --config "$root/about.toml" --manifest-path "$root/crates/cli/Cargo.toml" --locked \
    "$notices/about.hbs" > "$crates"
cargo run --quiet --locked --manifest-path "$notices/syntaxes/Cargo.toml" \
    --target-dir "${CARGO_TARGET_DIR:-$root/target}/syntax-notices" > "$syntaxes"

# Trailing spaces in some license texts are dropped, and the file ends with one newline.
{
    cat "$crates"
    printf '\n## Syntax definitions\n\n'
    printf '%s\n' "$(cat "$syntaxes")"
} | sed 's/[[:space:]]*$//' > "$out"
echo "wrote $out"
