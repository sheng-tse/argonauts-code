"""Tests for bot/commands.py, with GitHub made up. Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "bot"))
import commands  # noqa: E402
from gh_api import GithubError  # noqa: E402

REPO = "example/argonauts-code"
NUMBER = 7
SHA = "abc1234def5678abc1234def5678abc1234def56"
BOT = "argonauts-bot[bot]"
AUTHOR = "fork-author"
MAINTAINER = "maintainer"


def run(run_id, name, status="completed", conclusion="failure", workflow_id=None, created_at="2026-10-20T10:00:00Z"):
    """A workflow run as GitHub lists it, with only the fields the bot reads."""
    return {"id": run_id, "name": name, "status": status, "conclusion": conclusion if status == "completed" else None,
            "workflow_id": workflow_id or run_id, "created_at": created_at}


def bot_reply(sha=SHA, login=BOT):
    return {"user": {"login": login}, "body": f"Rerunning the failed jobs of CI.\n\n{commands.rerun_mark(sha)}"}


class FakeGithub:
    """A pull request by AUTHOR at SHA, its runs and comments, and what the bot asks of it."""

    def __init__(self, runs=(), comments=(), refuse=(), permission_fails=False):
        self.runs = list(runs)
        self.comments = list(comments)
        self.refuse = set(refuse)
        self.permission_fails = permission_fails
        self.posted = []

    def get(self, path):
        if path == f"repos/{REPO}/pulls/{NUMBER}":
            return {"head": {"sha": SHA}, "user": {"login": AUTHOR}}
        prefix = f"repos/{REPO}/collaborators/"
        if path.startswith(prefix) and path.endswith("/permission"):
            if self.permission_fails:
                raise GithubError("HTTP 404")
            login = path[len(prefix):-len("/permission")]
            return {"permission": "write" if login == MAINTAINER else "read"}
        raise AssertionError(f"unexpected GET {path}")

    def pages(self, path, key=None):
        if path.startswith(f"repos/{REPO}/actions/runs?"):
            assert f"head_sha={SHA}" in path and "event=pull_request" in path and key == "workflow_runs", path
            return list(self.runs)
        if path.startswith(f"repos/{REPO}/issues/{NUMBER}/comments"):
            return list(self.comments)
        raise AssertionError(f"unexpected listing {path}")

    def post(self, path, body=None):
        if path in self.refuse:
            raise GithubError("HTTP 403: This workflow run is not completed")
        self.posted.append((path, body))
        return None

    def delete(self, path):
        raise AssertionError(f"unexpected DELETE {path}")

    def acted(self):
        """The reruns and stops asked for, as the last part of each path and the run's id, in the run's order."""
        acted = [(path.rsplit("/", 1)[1], int(path.split("/")[-2])) for path, _ in self.posted if "/runs/" in path]
        return sorted(acted, key=lambda done: done[1])

    def replies(self):
        return [body["body"] for path, body in self.posted if path == f"repos/{REPO}/issues/{NUMBER}/comments"]


def ask(api, body, who):
    env = {"COMMENT_BODY": body, "COMMENTER": who, "GH_REPO": REPO, "PR_NUMBER": str(NUMBER), "BOT_LOGIN": BOT}
    return commands.answer(env, api)


class RerunTest(unittest.TestCase):
    def test_a_maintainer_reruns_what_failed_or_was_stopped(self):
        runs = [run(1, "CI"), run(2, "Packages", conclusion="cancelled"), run(3, "Labels", conclusion="success")]
        api = FakeGithub(runs=runs)
        reply = ask(api, "/bot rerun failed", MAINTAINER)
        self.assertEqual(api.acted(), [("rerun-failed-jobs", 1), ("rerun-failed-jobs", 2)])
        self.assertEqual(reply, "Rerunning the failed jobs of CI and Packages on `abc1234`.")
        self.assertEqual(api.replies(), [reply])

    def test_an_author_reruns_twice_a_commit_and_is_told_how_many_are_left(self):
        api = FakeGithub(runs=[run(1, "CI")])
        first = ask(api, "/bot rerun failed", AUTHOR)
        self.assertIn("You can ask for 1 more on this commit.", first)
        self.assertIn(commands.rerun_mark(SHA), first)

        api = FakeGithub(runs=[run(1, "CI")], comments=[bot_reply()])
        second = ask(api, "/bot rerun failed", AUTHOR)
        self.assertEqual(api.acted(), [("rerun-failed-jobs", 1)])
        self.assertIn("That was the last you can ask for.", second)

        api = FakeGithub(runs=[run(1, "CI")], comments=[bot_reply(), bot_reply()])
        third = ask(api, "/bot rerun failed", AUTHOR)
        self.assertEqual(api.acted(), [])
        self.assertTrue(third.startswith("You've asked for 2 reruns of `abc1234`, the most its author can."), third)
        self.assertNotIn(commands.rerun_mark(SHA), third)

    def test_only_the_bots_own_replies_about_this_commit_count(self):
        quoted = bot_reply(login=MAINTAINER)
        older = bot_reply(sha="0" * 40)
        api = FakeGithub(runs=[run(1, "CI")], comments=[quoted, older, bot_reply()])
        reply = ask(api, "/bot rerun failed", AUTHOR)
        self.assertEqual(api.acted(), [("rerun-failed-jobs", 1)])
        self.assertIn("That was the last you can ask for.", reply)

    def test_a_maintainer_has_no_limit(self):
        api = FakeGithub(runs=[run(1, "CI")], comments=[bot_reply(), bot_reply(), bot_reply()])
        reply = ask(api, "/bot rerun failed", MAINTAINER)
        self.assertEqual(api.acted(), [("rerun-failed-jobs", 1)])
        self.assertNotIn("<!--", reply)

    def test_an_author_reruns_a_run_that_timed_out_but_not_one_that_was_stopped(self):
        runs = [run(1, "CI", conclusion="cancelled"), run(2, "Packages", conclusion="timed_out")]
        api = FakeGithub(runs=runs)
        reply = ask(api, "/bot rerun failed", AUTHOR)
        self.assertEqual(api.acted(), [("rerun-failed-jobs", 2)])
        self.assertTrue(reply.startswith("Rerunning the failed jobs of Packages on `abc1234`."), reply)

    def test_an_author_cant_undo_a_maintainers_stop(self):
        api = FakeGithub(runs=[run(1, "CI", conclusion="cancelled")])
        self.assertEqual(ask(api, "/bot rerun failed", AUTHOR), "Nothing has failed on `abc1234`.")
        self.assertEqual(api.acted(), [])

    def test_only_the_newest_run_of_each_workflow_counts(self):
        older = run(1, "CI", workflow_id=10, created_at="2026-10-20T10:00:00Z")
        newer = run(2, "CI", workflow_id=10, conclusion="success", created_at="2026-10-20T11:00:00Z")
        api = FakeGithub(runs=[older, newer])
        self.assertEqual(ask(api, "/bot rerun failed", MAINTAINER), "Nothing has failed on `abc1234`.")
        self.assertEqual(api.acted(), [])

    def test_runs_still_going_are_named(self):
        api = FakeGithub(runs=[run(1, "CI", status="in_progress"), run(2, "Packages", conclusion="success")])
        reply = ask(api, "/bot rerun failed", AUTHOR)
        self.assertEqual(reply, "Nothing has failed on `abc1234`, and CI is still running.")
        self.assertNotIn(commands.rerun_mark(SHA), reply)

        both = [run(1, "CI", status="queued"), run(2, "Packages", status="in_progress")]
        reply = ask(FakeGithub(runs=both), "/bot rerun failed", MAINTAINER)
        self.assertEqual(reply, "Nothing has failed on `abc1234`, and CI and Packages are still running.")

    def test_someone_else_can_rerun_nothing(self):
        api = FakeGithub(runs=[run(1, "CI")])
        reply = ask(api, "/bot rerun failed", "passer-by")
        self.assertEqual(api.acted(), [])
        self.assertEqual(reply, "Only maintainers and this pull request's author can rerun its jobs.")

    def test_a_permission_github_wont_tell_is_no_permission(self):
        api = FakeGithub(runs=[run(1, "CI")], permission_fails=True)
        self.assertIn("Only maintainers", ask(api, "/bot rerun failed", MAINTAINER))
        self.assertEqual(api.acted(), [])

    def test_a_refused_rerun_is_reported_and_not_counted(self):
        refused = f"repos/{REPO}/actions/runs/1/rerun-failed-jobs"
        api = FakeGithub(runs=[run(1, "CI")], refuse=[refused])
        reply = ask(api, "/bot rerun failed", AUTHOR)
        self.assertEqual(reply, "Couldn't rerun CI: HTTP 403: This workflow run is not completed")
        self.assertNotIn(commands.rerun_mark(SHA), reply)


class StopTest(unittest.TestCase):
    def test_a_maintainer_stops_every_run_not_finished(self):
        runs = [run(1, "CI", status="in_progress"), run(2, "Packages", status="queued"), run(3, "Labels")]
        api = FakeGithub(runs=runs)
        reply = ask(api, "/bot stop", MAINTAINER)
        self.assertEqual(api.acted(), [("cancel", 1), ("cancel", 2)])
        self.assertEqual(reply, "Stopping CI and Packages on `abc1234`.")

    def test_an_author_cant_stop_runs(self):
        api = FakeGithub(runs=[run(1, "CI", status="in_progress")])
        self.assertEqual(ask(api, "/bot stop", AUTHOR), "Only maintainers can stop runs.")
        self.assertEqual(api.acted(), [])

    def test_nothing_running_is_said(self):
        api = FakeGithub(runs=[run(1, "CI")])
        self.assertEqual(ask(api, "/bot stop", MAINTAINER), "Nothing is running on `abc1234`.")


class CommandTest(unittest.TestCase):
    def test_a_comment_that_isnt_a_command_gets_no_reply(self):
        for body in ["", "Looks good", "/bottle", "Try /bot stop"]:
            api = FakeGithub()
            self.assertIsNone(ask(api, body, MAINTAINER), body)
            self.assertEqual(api.posted, [])

    def test_case_spacing_and_later_lines_dont_matter(self):
        self.assertEqual(commands.command_of("  /Bot  Rerun   FAILED\nThanks!"), "rerun failed")

    def test_an_unknown_command_gets_the_list(self):
        for body in ["/bot", "/bot rerun", "/bot merge"]:
            api = FakeGithub()
            self.assertEqual(ask(api, body, MAINTAINER), commands.HELP)
            self.assertEqual(api.acted(), [])

    def test_names_read_as_a_sentence(self):
        self.assertEqual(commands.names([run(1, "CI")]), "CI")
        self.assertEqual(commands.names([run(1, "CI"), run(2, "CI")]), "CI")
        self.assertEqual(commands.names([run(1, "A"), run(2, "B"), run(3, "C")]), "A, B and C")


if __name__ == "__main__":
    unittest.main()
