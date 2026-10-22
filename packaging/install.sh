#!/bin/sh
# Installs Argonauts Code: dist's own installer from the release, run as it is (its checksums, its paths, its
# messages), then the greeting once. https://argonauts-code.github.io/install.sh serves this file:
#
#   curl -fsSL https://argonauts-code.github.io/install.sh | sh
#   curl -fsSL https://argonauts-code.github.io/install.sh | sh -s -- --beta
#
# --beta installs the newest release, beta or stable. Every other option goes to dist's installer, such as
# --no-modify-path or --quiet. The greeting shows only on a terminal and never in CI.
set -eu

repo="sheng-tse/argonauts-code"
releases="https://github.com/${repo}/releases"
installer="argonauts-code-installer.sh"

say() {
    printf '%s\n' "$1"
}

die() {
    say "install.sh: $1" >&2
    exit 1
}

fetch() {
    if command -v curl > /dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -fsSL "$1"
    elif command -v wget > /dev/null 2>&1; then
        wget --https-only --secure-protocol=TLSv1_2 -qO- "$1"
    else
        die "curl or wget is needed to download Argonauts Code"
    fi
}

# The newest release of any kind is the first the releases API lists. A beta tag's release is a prerelease, which
# `releases/latest` never points at.
newest_tag() {
    fetch "https://api.github.com/repos/${repo}/releases?per_page=1" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' |
        head -n 1
}

# Where dist's installer put the binary, from the receipt it writes beside its settings.
installed_binary() {
    receipt="${XDG_CONFIG_HOME:-$HOME/.config}/argonauts-code/argonauts-code-receipt.json"
    if [ -f "$receipt" ]; then
        prefix=$(sed -n 's/.*"install_prefix":"\([^"]*\)".*/\1/p' "$receipt")
        for candidate in "$prefix/argonauts" "$prefix/bin/argonauts"; do
            if [ -x "$candidate" ]; then
                say "$candidate"
                return
            fi
        done
    fi
    command -v argonauts || true
}

beta=0
greet=1
count=$#
while [ "$count" -gt 0 ]; do
    arg=$1
    shift
    count=$((count - 1))
    case "$arg" in
        --beta)
            beta=1
            continue
            ;;
        -q | --quiet | -h | --help)
            greet=0
            ;;
    esac
    set -- "$@" "$arg"
done

if [ "$beta" = 1 ]; then
    tag=$(newest_tag)
    [ -n "$tag" ] || die "couldn't find the newest release at ${releases}"
    url="${releases}/download/${tag}/${installer}"
else
    url="${releases}/latest/download/${installer}"
fi

script=$(mktemp)
trap 'rm -f "$script"' EXIT
fetch "$url" > "$script" || die "couldn't download ${url}"
sh "$script" "$@"

if [ "$greet" = 1 ] && [ -t 1 ] && [ -z "${CI:-}" ]; then
    binary=$(installed_binary)
    if [ -n "$binary" ]; then
        "$binary" welcome || true
    fi
fi
