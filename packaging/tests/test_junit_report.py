"""Tests for .github/scripts/junit_report.py, on JUnit reports written the way cargo-nextest
writes them. Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import contextlib
import io
import pathlib
import sys
import tempfile
import unittest

# The script lives under .github, where no ignore rule keeps a bytecode cache out of git.
sys.dont_write_bytecode = True
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2] / ".github" / "scripts"))
import junit_report  # noqa: E402

PASSED = '<testcase name="{name}" classname="{binary}" time="0.010"></testcase>'
FLAKY = """<testcase name="{name}" classname="{binary}" time="1.200">
  <flakyFailure message="thread panicked: timed out waiting for the input" type="test failure">
    <system-out>the screen was:</system-out>
  </flakyFailure>
</testcase>"""
FAILED = """<testcase name="{name}" classname="{binary}" time="9.000">
  <failure message="assertion failed: left == right" type="test failure">left: 1</failure>
  <rerunFailure message="assertion failed: left == right" type="test failure"/>
  <rerunFailure message="assertion failed: left == right" type="test failure"/>
</testcase>"""


def report(*cases):
    body = "\n".join(kind.format(name=name, binary="argonauts-tui::it") for kind, name in cases)
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<testsuites name="nextest-run" tests="3" failures="1" errors="0">\n'
        f'<testsuite name="argonauts-tui::it" tests="3" failures="1" errors="0">\n{body}\n'
        "</testsuite>\n</testsuites>\n"
    )


class Reports(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def write(self, name, text):
        path = pathlib.Path(self.dir.name) / name
        path.write_text(text, encoding="utf-8")
        return path

    def run_report(self, *args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = junit_report.main([str(arg) for arg in args])
        return code, out.getvalue()

    def test_a_retry_that_passed_is_named_as_flaky(self):
        path = self.write(
            "junit.xml",
            report((PASSED, "pty_keys::ok"), (FLAKY, "pty_goals::the_view"), (FAILED, "pty_live::fast")),
        )
        code, out = self.run_report("--title", "Tests (macos-15, rest)", path)
        self.assertEqual(code, 0)
        self.assertIn("### Tests (macos-15, rest)", out)
        self.assertIn("3 tests in 1 run: 1 failed, 1 passed only on a retry.", out)
        self.assertIn("#### Flaky: passed only on a retry", out)
        self.assertIn("| `argonauts-tui::it pty_goals::the_view` | 1 of 1 | thread panicked", out)
        self.assertIn(
            "::warning title=Flaky test::argonauts-tui::it pty_goals::the_view passed only on a "
            "retry, once in 1 run",
            out,
        )
        self.assertIn(
            "::error title=Failed test::argonauts-tui::it pty_live::fast failed in 1 of 1 run", out
        )
        self.assertNotIn("pty_keys::ok", out)

    def test_runs_are_counted_across_reports(self):
        first = self.write("run-1.xml", report((PASSED, "a::b"), (PASSED, "c::d")))
        second = self.write("run-2.xml", report((FAILED, "a::b"), (PASSED, "c::d")))
        third = self.write("run-3.xml", report((FLAKY, "a::b"), (PASSED, "c::d")))
        code, out = self.run_report("--fail-on-failures", first, second, third)
        self.assertEqual(code, 1)
        self.assertIn("2 tests in 3 runs: 1 failed, 0 passed only on a retry.", out)
        self.assertIn("| `argonauts-tui::it a::b` | 1 of 3 |", out)
        self.assertNotIn("c::d", out)

    def test_clean_runs_pass_even_when_failures_fail(self):
        path = self.write("junit.xml", report((PASSED, "a::b")))
        code, out = self.run_report("--fail-on-failures", path)
        self.assertEqual(code, 0)
        self.assertIn("1 test in 1 run: 0 failed, 0 passed only on a retry.", out)
        self.assertNotIn("::", out.replace("a::b", ""))

    def test_a_missing_report_is_said(self):
        code, out = self.run_report(pathlib.Path(self.dir.name) / "nowhere.xml")
        self.assertEqual(code, 0)
        self.assertIn("No test report from", out)
        self.assertIn("the tests didn't run", out)

    def test_a_run_without_a_report_fails_a_hunt(self):
        path = self.write("run-1.xml", report((PASSED, "a::b")))
        code, out = self.run_report("--fail-on-failures", path, pathlib.Path(self.dir.name) / "run-2.xml")
        self.assertEqual(code, 1)
        self.assertIn("1 test in 1 run", out)

    def test_the_summary_goes_to_its_file_and_annotations_to_stdout(self):
        path = self.write("junit.xml", report((FLAKY, "a::b")))
        summary = pathlib.Path(self.dir.name) / "summary.md"
        summary.write_text("before\n", encoding="utf-8")
        code, out = self.run_report("--summary", summary, path)
        self.assertEqual(code, 0)
        written = summary.read_text(encoding="utf-8")
        self.assertTrue(written.startswith("before\n### Tests"), written)
        self.assertEqual(out.strip().splitlines()[0].split("::")[1], "warning title=Flaky test")

    def test_a_message_cannot_break_the_table(self):
        case = FAILED.replace("left == right", "a | b")
        path = self.write("junit.xml", report((case, "a::b")))
        _, out = self.run_report(path)
        self.assertIn("a \\| b", out)


if __name__ == "__main__":
    unittest.main()
