"""Checks that the update keys built into Argonauts Code are real minisign public keys.

A release that ships the placeholders could never check a manifest, so none of its installs could update to the
release that fixes it. Run from the repository root before a release:
    python3 packaging/updates/check_keys.py
"""
import base64
import binascii
import pathlib
import re
import sys

KEYS_FILE = pathlib.Path("crates/app/src/self_update/mod.rs")
PLACEHOLDER = "REPLACE-WITH-"


def built_in_keys(source):
    """The strings in PUBLIC_KEYS, in order."""
    found = re.search(r"pub const PUBLIC_KEYS: \[&str; \d+\] = \[(.*?)\];", source, re.DOTALL)
    if not found:
        raise ValueError("PUBLIC_KEYS isn't in the file")
    return re.findall(r'"([^"]*)"', found.group(1))


def problem(key):
    """Why `key` isn't a minisign public key, or None when it is one: base64 of "Ed", an 8-byte key id and a
    32-byte Ed25519 key."""
    if key.startswith(PLACEHOLDER):
        return "is still a placeholder"
    try:
        raw = base64.b64decode(key, validate=True)
    except (binascii.Error, ValueError):
        return "isn't base64"
    if len(raw) != 42 or raw[:2] != b"Ed":
        return "isn't a minisign public key"
    return None


def main(path=KEYS_FILE):
    keys = built_in_keys(path.read_text())
    problems = [f"key {at} {why}" for at, why in enumerate(map(problem, keys), 1) if why]
    if not keys:
        problems.append("there are no keys")
    for line in problems:
        print(f"::error::{path}: {line}; put your minisign public keys in PUBLIC_KEYS", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
