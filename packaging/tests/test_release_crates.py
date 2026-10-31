"""Tests for what the crates job and the Publishers check read from crates.io's index, against an index the tests
make up. Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import contextlib
import importlib.util
import io
import json
import pathlib
import unittest

RELEASE = pathlib.Path(__file__).resolve().parent.parent / "release"
SPEC = importlib.util.spec_from_file_location("crates", RELEASE / "crates.py")
crates = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(crates)

METADATA = {
    "workspace_members": ["core 0.2.1", "tui 0.2.1", "cli 0.2.1", "testkit 0.2.1"],
    "packages": [
        {"id": "core 0.2.1", "name": "argonauts-core", "version": "0.2.1", "publish": None},
        {"id": "tui 0.2.1", "name": "argonauts-tui", "version": "0.2.1", "publish": ["crates-io"]},
        {"id": "cli 0.2.1", "name": "argonauts-code", "version": "0.2.1", "publish": None},
        {"id": "testkit 0.2.1", "name": "argonauts-testkit", "version": "0.2.1", "publish": []},
        {"id": "outside 1.0.0", "name": "serde", "version": "1.0.0", "publish": None},
    ],
}
CRATES = crates.workspace_crates(METADATA)


def index(entries):
    """A made-up sparse index: each crate's versions, or no entry at all for a crate it has never had."""

    def get(url):
        for name, versions in entries.items():
            if url == f"{crates.INDEX}/{crates.index_path(name)}":
                return "".join(json.dumps({"name": name, "vers": version}) + "\n" for version in versions)
        return None

    return get


class IndexPaths(unittest.TestCase):
    def test_each_name_length_has_its_folder(self):
        self.assertEqual(crates.index_path("a"), "1/a")
        self.assertEqual(crates.index_path("ab"), "2/ab")
        self.assertEqual(crates.index_path("abc"), "3/a/abc")
        self.assertEqual(crates.index_path("Argonauts-Core"), "ar/go/argonauts-core")


class WhatToPublish(unittest.TestCase):
    def test_only_the_workspaces_crates_for_crates_io(self):
        self.assertEqual(
            CRATES, [("argonauts-core", "0.2.1"), ("argonauts-tui", "0.2.1"), ("argonauts-code", "0.2.1")]
        )

    def test_a_first_run_publishes_the_whole_workspace(self):
        get = index({"argonauts-core": ["0.2.0"], "argonauts-tui": ["0.2.0"], "argonauts-code": ["0.2.0"]})
        self.assertEqual(crates.to_publish(CRATES, get=get), (["--workspace"], []))

    def test_a_rerun_leaves_out_what_crates_io_has_at_this_version(self):
        get = index({"argonauts-core": ["0.2.0", "0.2.1"], "argonauts-tui": ["0.2.0"], "argonauts-code": ["0.2.1"]})
        args, there = crates.to_publish(CRATES, get=get)
        self.assertEqual(args, ["--workspace", "--exclude", "argonauts-core", "--exclude", "argonauts-code"])
        self.assertEqual(there, ["argonauts-core", "argonauts-code"])

    def test_nothing_is_left_once_crates_io_has_them_all(self):
        get = index({name: ["0.2.1"] for name, _ in CRATES})
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            self.assertEqual(crates.main(["to-publish"], METADATA, get), 0)
        self.assertEqual(out.getvalue(), "")
        self.assertIn("crates.io has all 3 crates, so there's nothing to publish.", err.getvalue())

    def test_the_arguments_go_out_one_per_line(self):
        get = index({"argonauts-core": ["0.2.1"], "argonauts-tui": [], "argonauts-code": ["0.2.0"]})
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            self.assertEqual(crates.main(["to-publish"], METADATA, get), 0)
        self.assertEqual(out.getvalue(), "--workspace\n--exclude\nargonauts-core\n")
        self.assertIn("Already on crates.io at 0.2.1: argonauts-core", err.getvalue())


class Existing(unittest.TestCase):
    def test_a_crate_crates_io_never_had_fails_with_what_to_do(self):
        get = index({"argonauts-core": ["0.2.0"], "argonauts-code": ["0.2.0"]})
        err = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            self.assertEqual(crates.main(["all-exist"], METADATA, get), 1)
        self.assertIn("argonauts-tui has never been on crates.io", err.getvalue())
        self.assertIn("publish its first version by hand", err.getvalue())

    def test_all_there_says_so(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(crates.main(["all-exist"], METADATA, index({n: ["0.1.0"] for n, _ in CRATES})), 0)
        self.assertEqual(out.getvalue(), "3 crates\tall already published, none new\n")

    def test_an_index_that_cant_be_read_is_not_run_rather_than_a_guess(self):
        def unreadable(url):
            raise crates.Unreadable(f"{url} answered 503")

        for command in ("to-publish", "all-exist"):
            err = io.StringIO()
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
                self.assertEqual(crates.main([command], METADATA, unreadable), 2, command)
            self.assertIn("answered 503", err.getvalue())

    def test_an_unknown_command_shows_the_usage(self):
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(crates.main(["publish"], METADATA), 2)
        self.assertIn("to-publish", err.getvalue())


if __name__ == "__main__":
    unittest.main()
