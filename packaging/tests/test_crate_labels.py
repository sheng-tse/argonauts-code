"""Tests for bot/crate_labels.py, on this repository's workspace and on made-up ones, with GitHub made up. Run from
the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import pathlib
import sys
import tempfile
import tomllib
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "bot"))
import crate_labels  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[2]
REPO = "example/argonauts-code"
NUMBER = 12
CRATES = {
    "crates/app": "argonauts-app",
    "crates/core": "argonauts-core",
    "crates/adapters/codex": "argonauts-adapter-codex",
    "crates/tui": "argonauts-tui",
}


def write_workspace(root, members, exclude=(), crates=()):
    """A workspace at `root` listing `members`, with a Cargo.toml naming each folder in `crates`."""
    listed = ", ".join(f'"{member}"' for member in members)
    left_out = ", ".join(f'"{folder}"' for folder in exclude)
    (root / "Cargo.toml").write_text(f"[workspace]\nmembers = [{listed}]\nexclude = [{left_out}]\n")
    for folder, name in crates:
        (root / folder).mkdir(parents=True, exist_ok=True)
        (root / folder / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\n')


class FakeGithub:
    """A pull request's files and labels, the repository's labels, and what the script asks of them."""

    def __init__(self, files=(), labels=(), on_pull=()):
        self.files = [file if isinstance(file, dict) else {"filename": file} for file in files]
        self.labels = list(labels)
        self.on_pull = list(on_pull)
        self.posted, self.deleted = [], []

    def pages(self, path, key=None):
        listings = {
            f"repos/{REPO}/pulls/{NUMBER}/files": self.files,
            f"repos/{REPO}/labels": [{"name": name} for name in self.labels],
            f"repos/{REPO}/issues/{NUMBER}/labels": [{"name": name} for name in self.on_pull],
        }
        listing = path.partition("?")[0]
        if listing not in listings:
            raise AssertionError(f"unexpected listing {path}")
        return listings[listing]

    def post(self, path, body=None):
        self.posted.append((path, body))

    def delete(self, path):
        self.deleted.append(path)

    def get(self, path):
        raise AssertionError(f"unexpected GET {path}")


class WorkspaceTest(unittest.TestCase):
    def test_every_member_of_this_workspace_is_a_crate(self):
        crates = crate_labels.workspace_crates(ROOT)
        self.assertEqual(crates["crates/tui"], "argonauts-tui")
        self.assertEqual(crates["crates/adapters/claude-code"], "argonauts-adapter-claude-code")
        self.assertEqual(crates["crates/cli"], "argonauts-code")
        members = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["members"]
        self.assertEqual(sorted(crates), sorted(members))

    def test_globs_exclusions_and_nested_crates(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            write_workspace(
                root,
                members=["crates/*", "crates/outer/inner"],
                exclude=["crates/scratch"],
                crates=[("crates/one", "one"), ("crates/outer", "outer"), ("crates/outer/inner", "inner"),
                        ("crates/scratch", "scratch")],
            )
            (root / "crates" / "notes").mkdir()
            crates = crate_labels.workspace_crates(root)
        self.assertEqual(crates, {"crates/one": "one", "crates/outer": "outer", "crates/outer/inner": "inner"})
        self.assertEqual(crate_labels.crate_of("crates/outer/inner/src/lib.rs", crates), "inner")
        self.assertEqual(crate_labels.crate_of("crates/outer/src/lib.rs", crates), "outer")


class MappingTest(unittest.TestCase):
    def test_a_path_belongs_to_the_crate_whose_folder_holds_it(self):
        self.assertEqual(crate_labels.crate_of("crates/tui/src/bottom/bar.rs", CRATES), "argonauts-tui")
        self.assertEqual(crate_labels.crate_of("crates/adapters/codex/fixtures/codex/turn.jsonl", CRATES),
                         "argonauts-adapter-codex")
        self.assertIsNone(crate_labels.crate_of("crates/tuition/src/lib.rs", CRATES))
        self.assertIsNone(crate_labels.crate_of("crates/tui", CRATES))
        self.assertIsNone(crate_labels.crate_of("Cargo.lock", CRATES))
        self.assertIsNone(crate_labels.crate_of(".github/workflows/ci.yml", CRATES))

    def test_a_moved_file_counts_on_both_sides(self):
        files = [{"filename": "crates/core/src/ways.rs", "previous_filename": "crates/app/src/ways.rs"},
                 {"filename": "README.md"}]
        self.assertEqual(crate_labels.labels_for(crate_labels.changed_paths(files), CRATES),
                         ["crate: argonauts-app", "crate: argonauts-core"])


class LabelTest(unittest.TestCase):
    def test_labels_follow_the_crates_a_pull_request_changes(self):
        api = FakeGithub(
            files=["crates/tui/src/lib.rs", "crates/tui/tests/golden.rs", "crates/core/src/lib.rs"],
            labels=["crate: argonauts-tui", "crate: argonauts-core", "crate: argonauts-app", "bug"],
            on_pull=["crate: argonauts-core", "crate: argonauts-app", "bug"],
        )
        said = crate_labels.label(api, REPO, NUMBER, CRATES)
        self.assertEqual(api.posted, [(f"repos/{REPO}/issues/{NUMBER}/labels", {"labels": ["crate: argonauts-tui"]})])
        self.assertEqual(api.deleted, [f"repos/{REPO}/issues/{NUMBER}/labels/crate%3A%20argonauts-app"])
        self.assertEqual(said, "Added crate: argonauts-tui. Took off crate: argonauts-app.")

    def test_a_label_the_repository_lacks_is_never_made_here(self):
        api = FakeGithub(files=["crates/tui/src/lib.rs", "crates/app/src/lib.rs"], labels=["crate: argonauts-app"])
        said = crate_labels.label(api, REPO, NUMBER, CRATES)
        self.assertEqual(api.posted, [(f"repos/{REPO}/issues/{NUMBER}/labels", {"labels": ["crate: argonauts-app"]})])
        self.assertEqual(said, "Added crate: argonauts-app. Missing from the repository, so not added: "
                               "crate: argonauts-tui.")

    def test_labels_that_already_match_are_left_alone(self):
        api = FakeGithub(files=["crates/app/src/lib.rs", "docs/notes.md"], labels=["crate: argonauts-app"],
                         on_pull=["crate: argonauts-app", "needs review"])
        said = crate_labels.label(api, REPO, NUMBER, CRATES)
        self.assertEqual(said, "The labels already match the crates it changes.")
        self.assertEqual((api.posted, api.deleted), ([], []))


class CreateTest(unittest.TestCase):
    def test_only_the_missing_labels_are_made(self):
        api = FakeGithub(labels=["crate: argonauts-app", "crate: argonauts-tui"])
        said = crate_labels.create(api, REPO, CRATES)
        made = [body for path, body in api.posted if path == f"repos/{REPO}/labels"]
        self.assertEqual(made, [
            {"name": "crate: argonauts-adapter-codex", "color": crate_labels.COLOR,
             "description": "Changes crates/adapters/codex"},
            {"name": "crate: argonauts-core", "color": crate_labels.COLOR, "description": "Changes crates/core"},
        ])
        self.assertEqual(said, "Made crate: argonauts-adapter-codex, crate: argonauts-core.")

    def test_nothing_to_make_is_said(self):
        api = FakeGithub(labels=[crate_labels.PREFIX + name for name in CRATES.values()])
        self.assertEqual(crate_labels.create(api, REPO, CRATES), "Every crate has its label already.")
        self.assertEqual(api.posted, [])


if __name__ == "__main__":
    unittest.main()
