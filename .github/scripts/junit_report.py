"""Says which tests failed and which passed only on a retry, from cargo-nextest's JUnit reports.

    python3 -I .github/scripts/junit_report.py [--title TITLE] [--summary FILE]
        [--fail-on-failures] REPORT...

Each REPORT is one run's junit.xml. It adds Markdown for the job's summary to FILE (stdout
without one) and prints a GitHub annotation for each test that failed or was flaky, so a test
that passed only on a retry is still named on the run's page. With several reports, as the
weekly flake hunt writes, it counts the runs each test failed in. A missing report is said,
not an error: the build failed before the tests ran, and that step failed already.

With --fail-on-failures it exits 1 when any test failed in any run, flaky ones included, or
when a run left no report.
"""

import argparse
import sys
import xml.etree.ElementTree as ElementTree
from dataclasses import dataclass
from pathlib import Path

# Lines of a failure's message kept in the summary.
MESSAGE_LINES = 3


@dataclass
class Outcome:
    """What one test did across the runs read."""

    runs: int = 0
    # Runs it failed in, retries and all.
    failed: int = 0
    # Runs it passed in only on a retry.
    flaky: int = 0
    # The first message of a failed try, cut short.
    message: str = ""


def tag(element):
    """An element's name without its namespace, in lower case."""
    return element.tag.rsplit("}", 1)[-1].lower()


def first_lines(text):
    lines = [line.rstrip() for line in (text or "").strip().splitlines() if line.strip()]
    return "\n".join(lines[:MESSAGE_LINES])


def message_of(element):
    """A failure's message, or the start of its text when it has none."""
    return first_lines(element.get("message") or element.text or "")


def read(report, outcomes):
    """Adds one run's report to `outcomes`, keyed by binary and test name."""
    for case in ElementTree.parse(report).iter():
        if tag(case) != "testcase":
            continue
        name = f"{case.get('classname', '')} {case.get('name', '')}".strip()
        outcome = outcomes.setdefault(name, Outcome())
        outcome.runs += 1
        failures = [child for child in case if tag(child) in ("failure", "error")]
        # nextest writes a passed retry's earlier tries as flakyFailure and a failed test's
        # earlier tries as rerunFailure, as Maven Surefire does.
        flaky = [child for child in case if tag(child).startswith("flaky")]
        reruns = [child for child in case if tag(child).startswith("rerun")]
        if failures:
            outcome.failed += 1
        elif flaky:
            outcome.flaky += 1
        else:
            continue
        if not outcome.message:
            first = (reruns or flaky or failures)[0]
            outcome.message = message_of(first)


def times(count):
    return "once" if count == 1 else f"{count} times"


def counted(count, word):
    return f"{count} {word}" if count == 1 else f"{count} {word}s"


def summary(title, outcomes, runs, missing):
    """The job summary's Markdown."""
    failed = {name: o for name, o in outcomes.items() if o.failed}
    flaky = {name: o for name, o in outcomes.items() if o.flaky and not o.failed}
    out = [f"### {title}", ""]
    if missing:
        out.append(f"No test report from {', '.join(missing)}: the tests didn't run.")
        out.append("")
    if runs:
        out.append(
            f"{counted(len(outcomes), 'test')} in {counted(runs, 'run')}: "
            f"{len(failed)} failed, {len(flaky)} passed only on a retry."
        )
        out.append("")
    for heading, chosen in (("Failed", failed), ("Flaky: passed only on a retry", flaky)):
        if not chosen:
            continue
        out.append(f"#### {heading}")
        out.append("")
        out.append("| Test | Runs | Message |")
        out.append("| --- | --- | --- |")
        for name in sorted(chosen):
            outcome = chosen[name]
            bad = outcome.failed or outcome.flaky
            message = outcome.message.replace("|", "\\|").replace("\n", "<br>")
            out.append(f"| `{name}` | {bad} of {outcome.runs} | {message} |")
        out.append("")
    return "\n".join(out)


def annotations(outcomes):
    """GitHub's workflow commands, one for each test that failed or was flaky."""
    lines = []
    for name in sorted(outcomes):
        outcome = outcomes[name]
        if outcome.failed:
            lines.append(
                f"::error title=Failed test::{name} failed in {outcome.failed} of "
                f"{counted(outcome.runs, 'run')}"
            )
        elif outcome.flaky:
            lines.append(
                f"::warning title=Flaky test::{name} passed only on a retry, "
                f"{times(outcome.flaky)} in {counted(outcome.runs, 'run')}"
            )
    return lines


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--title", default="Tests")
    parser.add_argument("--summary", type=Path)
    parser.add_argument("--fail-on-failures", action="store_true")
    parser.add_argument("reports", nargs="+", type=Path)
    args = parser.parse_args(argv)
    outcomes = {}
    missing = []
    runs = 0
    for report in args.reports:
        if not report.is_file():
            missing.append(str(report))
            continue
        read(report, outcomes)
        runs += 1
    text = summary(args.title, outcomes, runs, missing) + "\n"
    if args.summary:
        with args.summary.open("a", encoding="utf-8") as file:
            file.write(text)
    else:
        sys.stdout.write(text)
    for line in annotations(outcomes):
        print(line)
    bad = missing or any(o.failed or o.flaky for o in outcomes.values())
    return 1 if args.fail_on_failures and bad else 0


if __name__ == "__main__":
    sys.exit(main())
