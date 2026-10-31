"""Checks that CI stops a test that hangs only after the longest wait a scenario may take on the
slowest runner, and that every try of it still fits in the test job's time with room for the
build, the steps after it and the drawings step's own limit. Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import pathlib
import re
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]

# Room in a test job for all but the tries of one hung test and the drawings step: a cold
# build, the doc tests and the world's timed tests.
ROOM_MINUTES = 30

# The scenarios shard's step that builds the release binaries and compares every approved
# drawing with them, under a limit of its own.
DRAWINGS_STEP = "The approved drawings on the release build"


def ci_profile():
    """The ci profile's settings, over the default profile's it inherits."""
    profiles = tomllib.loads((ROOT / ".config" / "nextest.toml").read_text())["profile"]
    return {**profiles["default"], **profiles["ci"]}


def stopped_after(profile):
    """The seconds a test runs before nextest stops it."""
    slow = profile["slow-timeout"]
    period = re.fullmatch(r"(\d+)s", slow["period"])
    return int(period.group(1)) * slow["terminate-after"]


def ci_test_job():
    """The CI test job's lines."""
    lines = (ROOT / ".github" / "workflows" / "ci.yml").read_text().splitlines()
    start = lines.index("  test:")
    end = next(at for at in range(start + 1, len(lines)) if re.match(r"  [a-z0-9_-]+:$", lines[at]))
    return lines[start:end]


def job_minutes(lines):
    """The job's own limit, not a step's."""
    return next(int(m.group(1)) for line in lines if (m := re.fullmatch(r"    timeout-minutes: (\d+)", line)))


def step_minutes(lines, name):
    """The limit of the step called `name`, which must have one."""
    start = lines.index(f"      - name: {name}")
    end = next((at for at in range(start + 1, len(lines)) if lines[at].startswith("      - ")), len(lines))
    return next(int(m.group(1)) for line in lines[start:end] if (m := re.fullmatch(r"        timeout-minutes: (\d+)", line)))


def largest_scale(lines):
    """The highest deadline scale any runner of the job gets."""
    line = next(line for line in lines if "ARGONAUTS_TEST_DEADLINE_SCALE:" in line)
    return max(int(scale) for scale in re.findall(r"'(\d+)'", line))


def longest_scenario_wait():
    """The longest base wait a scenario names, in seconds."""
    waits = [
        int(seconds)
        for path in (ROOT / "crates" / "scenarios").rglob("*.rs")
        for seconds in re.findall(r"const \w*WAIT: Duration = Duration::from_secs\((\d+)\);", path.read_text())
    ]
    return max(waits)


class HungTests(unittest.TestCase):
    def test_a_hung_test_is_stopped_only_past_a_scenarios_longest_wait(self):
        longest = longest_scenario_wait() * largest_scale(ci_test_job())
        self.assertGreater(stopped_after(ci_profile()), longest)

    def test_every_try_of_a_hung_test_fits_in_the_job(self):
        profile = ci_profile()
        tries = profile["retries"] + 1
        job = ci_test_job()
        needed = tries * stopped_after(profile) / 60 + ROOM_MINUTES + step_minutes(job, DRAWINGS_STEP)
        self.assertLessEqual(needed, job_minutes(job))


if __name__ == "__main__":
    unittest.main()
