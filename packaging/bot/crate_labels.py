#!/usr/bin/env python3
"""Gives a pull request a `crate: <name>` label for each crate it changes. .github/workflows/labels.yml runs it on
every push to a pull request:

    python3 -I packaging/bot/crate_labels.py label

A crate is a member of the workspace in Cargo.toml, named as its own Cargo.toml names it, so a label reads as the
name `cargo test -p` takes. A pull request changes a crate when a file it adds, edits, removes or moves away sits in
that crate's folder. The labels follow the pull request: a crate a later push no longer changes loses its label.
Only labels the repository already has are added, and

    python3 -I packaging/bot/crate_labels.py create

makes the missing ones, which the workflow does only when it's run by hand.

The pull request comes from GH_REPO and PR_NUMBER.
"""

import os
import pathlib
import sys
import tomllib
import urllib.parse

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from gh_api import Github  # noqa: E402

PREFIX = "crate: "
COLOR = "c5def5"
ROOT = pathlib.Path(__file__).resolve().parents[2]


def workspace_crates(root):
    """Each workspace member's folder, relative to the root, and the name of the crate in it."""
    workspace = tomllib.loads((root / "Cargo.toml").read_text()).get("workspace", {})
    excluded = {(root / path).resolve() for path in workspace.get("exclude", [])}
    crates = {}
    for member in workspace.get("members", []):
        for folder in sorted(root.glob(member)):
            manifest = folder / "Cargo.toml"
            if folder.resolve() in excluded or not manifest.is_file():
                continue
            name = tomllib.loads(manifest.read_text()).get("package", {}).get("name")
            if name:
                crates[folder.relative_to(root).as_posix()] = name
    return crates


def crate_of(path, crates):
    """The crate whose folder holds the path, the innermost one where folders nest, or None."""
    holding = [folder for folder in crates if path.startswith(folder + "/")]
    return crates[max(holding, key=len)] if holding else None


def changed_paths(files):
    """Every path a pull request's file list names: a moved file counts where it left as well as where it went."""
    for found in files:
        yield found["filename"]
        if found.get("previous_filename"):
            yield found["previous_filename"]


def labels_for(paths, crates):
    return sorted({PREFIX + name for name in (crate_of(path, crates) for path in paths) if name})


def names_of(labels):
    return {label["name"] for label in labels}


def label(api, repo, number, crates):
    """Makes the pull request's crate labels match the crates it changes. Returns what it did, as one line."""
    # GitHub lists at most 3,000 of a pull request's files, which still names every crate a reviewable one changes.
    wanted = set(labels_for(changed_paths(api.pages(f"repos/{repo}/pulls/{number}/files?per_page=100")), crates))
    existing = names_of(api.pages(f"repos/{repo}/labels?per_page=100"))
    current = names_of(api.pages(f"repos/{repo}/issues/{number}/labels?per_page=100"))

    adding = sorted((wanted & existing) - current)
    removing = sorted(name for name in current - wanted if name.startswith(PREFIX))
    missing = sorted(wanted - existing)
    if adding:
        api.post(f"repos/{repo}/issues/{number}/labels", {"labels": adding})
    for name in removing:
        api.delete(f"repos/{repo}/issues/{number}/labels/{urllib.parse.quote(name, safe='')}")

    said = []
    if adding:
        said.append(f"Added {', '.join(adding)}.")
    if removing:
        said.append(f"Took off {', '.join(removing)}.")
    if missing:
        said.append(f"Missing from the repository, so not added: {', '.join(missing)}.")
    return " ".join(said) or "The labels already match the crates it changes."


def create(api, repo, crates):
    """Makes the label of every crate that has none yet. Returns what it did, as one line."""
    existing = names_of(api.pages(f"repos/{repo}/labels?per_page=100"))
    made = []
    for folder, name in sorted(crates.items(), key=lambda crate: crate[1]):
        if PREFIX + name in existing:
            continue
        api.post(f"repos/{repo}/labels", {"name": PREFIX + name, "color": COLOR, "description": f"Changes {folder}"})
        made.append(PREFIX + name)
    return f"Made {', '.join(made)}." if made else "Every crate has its label already."


if __name__ == "__main__":
    action = sys.argv[1:]
    if action == ["label"]:
        print(label(Github(), os.environ["GH_REPO"], int(os.environ["PR_NUMBER"]), workspace_crates(ROOT)))
    elif action == ["create"]:
        print(create(Github(), os.environ["GH_REPO"], workspace_crates(ROOT)))
    else:
        sys.exit("Usage: crate_labels.py label | create")
