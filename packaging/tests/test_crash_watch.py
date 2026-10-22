"""Tests for crashes/watch.py, with the query's rows and gh made up. Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "crashes"))
import watch  # noqa: E402

ENV = {"POSTHOG_QUERY_KEY": "phx_test", "POSTHOG_PROJECT_ID": "1", "GH_REPO": "example/argonauts-code"}


def rows(*versions):
    """Rows as the query groups them: (version, crashes by place as JSON, reports), for each version a number of
    reports without crashes and a number with one crash in `place`."""
    found = []
    for name, quiet, crashed, place in versions:
        found.append((name, "{}", quiet))
        if crashed:
            found.append((name, f'{{"{place}": 1}}', crashed))
    return found


class Gh:
    """Records the issues a run opens, with the titles of the ones already open."""

    def __init__(self, open_titles=()):
        self.open_titles = list(open_titles)
        self.created = []

    def open_titled(self, title, repo):
        return [found for found in self.open_titles if found == title]

    def create(self, title, body, repo):
        self.created.append((title, body, repo))


def run(env, found, gh):
    return watch.watch(env, query=lambda host, project, key: found, open_titled=gh.open_titled, create=gh.create)


class WatchTest(unittest.TestCase):
    def test_a_newest_version_that_crashes_more_opens_an_issue(self):
        gh = Gh()
        said = run(ENV, rows(("0.1.1", 90, 10, "tui::render"), ("0.1.0", 98, 2, "app::start")), gh)
        self.assertIn("Opened an issue", said)
        [(title, body, repo)] = gh.created
        self.assertEqual(title, "Crashes rose in 0.1.1")
        self.assertEqual(repo, "example/argonauts-code")
        self.assertIn("| 0.1.1 | 100 | 10 | 0.100 |", body)
        self.assertIn("| 0.1.0 | 100 | 2 | 0.020 |", body)
        self.assertIn("- `tui::render`: 10", body)

    def test_crashes_that_hold_steady_open_nothing(self):
        gh = Gh()
        said = run(ENV, rows(("0.1.1", 95, 5, "tui::render"), ("0.1.0", 96, 4, "tui::render")), gh)
        self.assertEqual(gh.created, [])
        self.assertIn("0.1.1: 0.050 crashes a report", said)

    def test_a_rare_crash_doubling_is_no_rise(self):
        gh = Gh()
        run(ENV, rows(("0.1.1", 98, 2, "app::start"), ("0.1.0", 99, 1, "app::start")), gh)
        self.assertEqual(gh.created, [])

    def test_a_version_with_few_reports_says_nothing_yet(self):
        gh = Gh()
        said = run(ENV, rows(("0.1.2", 5, 5, "app::start"), ("0.1.1", 95, 5, "x"), ("0.1.0", 96, 4, "x")), gh)
        self.assertEqual(gh.created, [])
        self.assertIn("0.1.1:", said)

    def test_an_issue_already_open_is_left_as_it_is(self):
        gh = Gh(open_titles=["Crashes rose in 0.1.1"])
        said = run(ENV, rows(("0.1.1", 90, 10, "tui::render"), ("0.1.0", 98, 2, "app::start")), gh)
        self.assertIn("already open", said)
        self.assertEqual(gh.created, [])

    def test_without_the_key_nothing_is_asked(self):
        def query(host, project, key):
            raise AssertionError("asked PostHog without a key")

        said = watch.watch({"GH_REPO": "example/argonauts-code"}, query=query)
        self.assertIn("No POSTHOG_QUERY_KEY", said)

    def test_versions_order_as_semver_and_a_build_of_no_release_is_left_out(self):
        versions = watch.versions_of(
            rows(("0.2.0-beta.1", 30, 0, ""), ("0.1.10", 30, 0, ""), ("0.1.9", 30, 0, ""), ("dev", 30, 0, ""))
        )
        self.assertNotIn("dev", versions)
        newest, before = watch.newest_two(versions)
        self.assertEqual((newest.name, before.name), ("0.2.0-beta.1", "0.1.10"))

    def test_a_crash_map_reads_from_text_or_an_object(self):
        self.assertEqual(watch.crash_map('{"a": 2, "b": 0}'), {"a": 2})
        self.assertEqual(watch.crash_map({"a": 1}), {"a": 1})
        self.assertEqual(watch.crash_map(None), {})
        self.assertEqual(watch.crash_map("not json"), {})


if __name__ == "__main__":
    unittest.main()
