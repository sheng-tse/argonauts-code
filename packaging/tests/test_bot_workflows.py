"""Checks that the bot's workflows keep their safety rules: no step runs the pull request's code, text from the
event reaches a script only through an environment variable, and each token may do only what its job needs. Run
from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import pathlib
import re
import unittest

WORKFLOWS = pathlib.Path(__file__).resolve().parents[2] / ".github" / "workflows"


def lines_of(name):
    return (WORKFLOWS / name).read_text().splitlines()


def run_blocks(lines):
    """Each `run:` step's script, the line itself and every line indented deeper under it."""
    blocks = []
    for at, line in enumerate(lines):
        if not re.match(r"\s*(- )?run:", line):
            continue
        indent = len(line) - len(line.lstrip(" -"))
        block = [line]
        for later in lines[at + 1:]:
            if later.strip() and len(later) - len(later.lstrip()) <= indent:
                break
            block.append(later)
        blocks.append("\n".join(block))
    return blocks


def jobs(lines):
    """Each job's lines, by the job's id."""
    found, current = {}, None
    for line in lines[lines.index("jobs:") + 1:]:
        named = re.match(r"  ([a-z0-9_-]+):$", line)
        if named:
            current = found.setdefault(named.group(1), [])
        elif current is not None:
            current.append(line)
    return found


def token_permissions(lines):
    """What the app token a job makes may do."""
    return sorted(line.strip() for line in lines if line.strip().startswith("permission-"))


class Rules:
    """The rules every bot workflow keeps; each test class names its file."""

    name = None

    def setUp(self):
        self.lines = lines_of(self.name)
        self.text = "\n".join(self.lines)

    def test_no_script_reads_the_event_in_its_text(self):
        for block in run_blocks(self.lines):
            self.assertNotIn("${{", block)

    def test_no_step_checks_out_the_pull_requests_code(self):
        self.assertIsNone(re.search(r"^\s+ref:", self.text, re.MULTILINE))
        self.assertNotIn("head.sha", self.text)
        self.assertNotIn("head_ref", self.text)
        self.assertEqual(self.text.count("persist-credentials: false"), self.text.count("actions/checkout@"))

    def test_the_workflows_own_token_may_only_read_the_code(self):
        self.assertIn("\npermissions: {}\n", self.text)
        granted = {line.strip() for line in self.lines if re.match(r"\s+[a-z-]+: (read|write)$", line)}
        self.assertEqual({line for line in granted if not line.startswith("permission-")}, {"contents: read"})


class BotTest(Rules, unittest.TestCase):
    name = "bot.yml"

    def test_the_comment_reaches_the_script_only_through_its_variable(self):
        uses = [line.strip() for line in self.lines if "${{" in line and "comment.body" in line]
        self.assertEqual(uses, ["COMMENT_BODY: ${{ github.event.comment.body }}"])

    def test_only_a_collaborator_or_the_pull_requests_author_starts_the_job(self):
        condition = " ".join(jobs(self.lines)["command"])
        self.assertIn("github.event.comment.author_association", condition)
        self.assertIn("github.event.comment.user.login == github.event.issue.user.login", condition)

    def test_one_pull_requests_commands_are_answered_one_at_a_time(self):
        job = "\n".join(jobs(self.lines)["command"])
        self.assertIn("    concurrency:\n      group: bot-${{ github.event.issue.number }}", job)
        self.assertIn("      cancel-in-progress: false", job)

    def test_the_token_may_rerun_stop_and_comment(self):
        self.assertEqual(list(jobs(self.lines)), ["command"])
        allowed = ["permission-actions: write", "permission-pull-requests: write"]
        self.assertEqual(token_permissions(self.lines), allowed)


class LabelsTest(Rules, unittest.TestCase):
    name = "labels.yml"

    def test_each_job_has_a_token_of_its_own(self):
        found = jobs(self.lines)
        self.assertEqual(sorted(found), ["create", "label"])
        self.assertEqual(token_permissions(found["label"]), ["permission-pull-requests: write"])
        self.assertEqual(token_permissions(found["create"]), ["permission-issues: write"])

    def test_labels_are_made_only_when_the_workflow_is_run_by_hand(self):
        found = jobs(self.lines)
        self.assertIn("    if: github.event_name == 'workflow_dispatch'", found["create"])
        self.assertIn("    if: github.event_name == 'pull_request_target'", found["label"])
        self.assertNotIn("crate_labels.py create", "\n".join(found["label"]))


if __name__ == "__main__":
    unittest.main()
