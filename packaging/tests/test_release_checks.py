"""Tests for the release checks (packaging/release/checks.sh) and the rehearsal that runs them all on one commit,
in a repository the tests make up, with a made-up clock, gh, curl, cargo and df. Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import datetime
import os
import pathlib
import shutil
import subprocess
import tempfile
import textwrap
import unittest

RELEASE = pathlib.Path(__file__).resolve().parent.parent / "release"
CHECKS = RELEASE / "checks.sh"
REHEARSE = RELEASE / "rehearse.sh"
# 2026-11-01 12:00 UTC.
NOW = 1793534400
REAL_DATE = shutil.which("date")

FAKES = {
    # Only the clock is made up: the dates around it come from the system's own date.
    "date": f"""
        if [ "$*" = "-u +%s" ]; then echo "$FAKE_NOW"; exit 0; fi
        exec {REAL_DATE} "$@"
    """,
    # Answers from files the test writes: gh.ci (CI's passed runs), gh.runs-<commit> and gh.runs-main (Release dry
    # runs), gh.job-<run> (the Publishers job's conclusion and address).
    "gh": """
        case "$*" in
            *"--workflow ci.yml"*) cat "$FAKE/gh.ci" ;;
            *"--workflow release.yml --commit"*) cat "$FAKE/gh.runs-commit" 2> /dev/null || true ;;
            *"--workflow release.yml --branch main"*) cat "$FAKE/gh.runs-main" 2> /dev/null || true ;;
            "run view "*) cat "$FAKE/gh.job-$3" ;;
            *) exit 1 ;;
        esac
    """,
    "curl": """
        out=""
        while [ $# -gt 0 ]; do
            case $1 in -o) out=$2; shift ;; esac
            shift
        done
        cat "$FAKE/served" > "$out" 2> /dev/null || true
        cat "$FAKE/status"
    """,
    "cargo": """
        case "$*" in
            "about --version") echo "cargo-about ${FAKE_ABOUT:-0.9.2}" ;;
            "deny --version") echo "cargo-deny 0.20.2" ;;
            "deny --locked check licenses advisories sources") ;;
            "publish --workspace --dry-run --locked")
                echo "$CARGO_PROFILE_DEV_DEBUG $CARGO_INCREMENTAL" > "$FAKE/cargo.env"
                printf '   Packaging argonauts-core v0.2.1\\n   Packaging argonauts-code v0.2.1\\n' ;;
            *) exit 1 ;;
        esac
    """,
    "df": """
        echo "Filesystem 1024-blocks Used Available Capacity Mounted on"
        echo "disk 1000000000 1 ${FAKE_FREE_KB:-500000000} 1% /"
    """,
}



def crates_py(status):
    """Stands in for packaging/release/crates.py in the made-up tree, answering `all-exist` as the real one does
    from crates.io's index: 0 when it has every crate, 1 naming one it has never had, 2 when it can't be read."""
    say = {
        0: 'print("2 crates\\tall already published, none new")',
        1: 'print("argonauts-world has never been on crates.io", file=sys.stderr)',
        2: 'print("crates.io\'s index: https://index.crates.io/ar/go/argonauts-core answered 503", file=sys.stderr)',
    }[status]
    return f"import sys\nassert sys.argv[1:] == ['all-exist']\n{say}\nsys.exit({status})\n"


CI_YML = "jobs:\n  licenses:\n    steps:\n      - uses: taiki-e/install-action@abc\n        with:\n          tool: cargo-deny@0.20.2\n"
BADGE = "[![Release](https://img.shields.io/badge/release-{tag}-f0a441?style=flat-square)](https://example.com)\n"


def day(offset):
    return (datetime.datetime.fromtimestamp(NOW, datetime.timezone.utc) + datetime.timedelta(days=offset)).strftime(
        "%Y-%m-%d"
    )


class Tree(unittest.TestCase):
    """A repository root holding what the checks read, and the fakes first on PATH."""

    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        self.fake = self.root / "fake"
        self.repo = self.root / "repo"
        (self.fake / "bin").mkdir(parents=True)
        for name, body in FAKES.items():
            path = self.fake / "bin" / name
            path.write_text("#!/bin/sh\n" + textwrap.dedent(body))
            path.chmod(0o755)
        self.write("CHANGELOG.md", f"# Changelog\n\n## [0.2.1] - {day(0)}\n\n## [0.2.0] - 2026-10-30\n")
        self.write("README.md", BADGE.format(tag="v0.2.1"))
        self.write(".github/workflows/ci.yml", CI_YML)
        self.write("packaging/updates/check_keys.py", "")
        self.write("packaging/install.sh", "echo install\n")
        self.write("THIRD_PARTY_NOTICES.md", "notices\n")
        self.write("packaging/notices/generate.sh", "true\n")
        self.write("packaging/release/crates.py", crates_py(0))
        self.answer("gh.ci", "1\n")
        self.answer("served", "echo install\n")
        self.answer("status", "200")
        self.env = {
            **os.environ,
            "PATH": f"{self.fake / 'bin'}:{os.environ['PATH']}",
            "FAKE": str(self.fake),
            "FAKE_NOW": str(NOW),
            "TMPDIR": str(self.root),
        }
        for name in ("GITHUB_ACTIONS", "GITHUB_REF", "GITHUB_REPOSITORY", "NO_COLOR"):
            self.env.pop(name, None)

    def write(self, name, text):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def answer(self, name, text):
        (self.fake / name).write_text(text)

    def check(self, *args, **env):
        done = subprocess.run(
            ["sh", str(CHECKS), *args],
            cwd=self.repo,
            env={**self.env, **env},
            capture_output=True,
            text=True,
        )
        return done.returncode, done.stdout, done.stderr


class Changelog(Tree):
    def dated(self, offset):
        self.write("CHANGELOG.md", f"## [0.2.1] - {day(offset)}\n")

    def test_today_and_yesterday_in_utc_pass_and_say_which(self):
        self.dated(0)
        self.assertEqual(self.check("changelog", "v0.2.1")[:2], (0, f'CHANGELOG has "## [0.2.1] - {day(0)}"\ttoday in UTC\n'))
        self.dated(-1)
        status, out, _ = self.check("changelog", "v0.2.1")
        self.assertEqual((status, out.split("\t")[1]), (0, "yesterday in UTC, allowed\n"))

    def test_any_other_day_fails_naming_the_days_that_would_pass(self):
        for offset in (-2, 1):
            self.dated(offset)
            status, _, err = self.check("changelog", "v0.2.1")
            self.assertEqual(status, 1)
            self.assertIn(f"needs {day(0)} or {day(-1)} (UTC)", err)

    def test_a_missing_section_names_the_heading_to_add(self):
        self.write("CHANGELOG.md", "## [0.2.0] - 2026-10-30\n")
        status, _, err = self.check("changelog", "v0.2.1")
        self.assertEqual(status, 1)
        self.assertIn(f'needs a section headed "## [0.2.1] - {day(0)}"', err)

    def test_a_rehearsal_takes_a_day_ahead_and_says_when_release_may_run(self):
        self.dated(1)
        status, out, _ = self.check("changelog", "v0.2.1", "--ahead")
        self.assertEqual((status, out.split("\t")[1]), (0, "Release may run that day or the next (UTC)\n"))
        self.dated(-1)
        self.assertEqual(self.check("changelog", "v0.2.1", "--ahead")[1].split("\t")[1], "Release must run today (UTC)\n")
        self.dated(-2)
        self.assertEqual(self.check("changelog", "v0.2.1", "--ahead")[0], 1)

    def test_a_beta_needs_no_section(self):
        self.write("CHANGELOG.md", "")
        self.assertEqual(self.check("changelog", "v0.3.0-beta.1")[0], 0)

    def test_in_github_actions_a_failure_is_an_annotation(self):
        self.dated(-3)
        self.assertTrue(self.check("changelog", "v0.2.1", GITHUB_ACTIONS="true")[2].startswith("::error::"))


class Badge(Tree):
    def test_the_badge_names_the_tag(self):
        self.assertEqual(self.check("badge", "v0.2.1")[:2], (0, "README's release badge names v0.2.1\t\n"))

    def test_an_old_badge_says_what_to_change_it_to(self):
        self.write("README.md", BADGE.format(tag="v0.2.0"))
        status, _, err = self.check("badge", "v0.2.1")
        self.assertEqual(status, 1)
        self.assertIn("says v0.2.0: make it https://img.shields.io/badge/release-v0.2.1-f0a441?style=flat-square", err)

    def test_a_beta_leaves_the_badge_alone(self):
        self.assertEqual(self.check("badge", "v0.3.0-beta.1")[0], 0)


class Runs(Tree):
    def test_ci_on_the_commit(self):
        self.assertEqual(self.check("ci-passed", "0123456789abcdef")[:2], (0, "CI passed on 012345678\t\n"))
        self.answer("gh.ci", "0\n")
        status, _, err = self.check("ci-passed", "0123456789abcdef")
        self.assertEqual(status, 1)
        self.assertIn("CI hasn't passed on 012345678", err)

    def test_gh_that_cant_answer_means_not_run(self):
        os.remove(self.fake / "gh.ci")
        status, _, err = self.check("ci-passed", "0123456789abcdef")
        self.assertEqual(status, 2)
        self.assertIn("not run:", err)

    def test_from_main_only(self):
        self.assertEqual(self.check("from-main", GITHUB_REF="refs/heads/main")[0], 0)
        status, _, err = self.check("from-main", GITHUB_REF="refs/heads/release-0.2.1")
        self.assertEqual(status, 1)
        self.assertIn("this run is on refs/heads/release-0.2.1", err)

    def test_publishers_from_a_dry_run_on_the_commit(self):
        self.answer("gh.runs-commit", "7\n")
        self.answer("gh.job-7", "success https://example.com/job\n")
        self.assertEqual(self.check("publishers", "0123456789abcdef")[:2], (0, "Publishers\tfrom the Release dry run on 012345678\n"))

    def test_publishers_from_a_dry_run_on_main_today(self):
        self.answer("gh.runs-commit", "6\n")
        self.answer("gh.job-6", "null null\n")
        self.answer("gh.runs-main", "8\n")
        self.answer("gh.job-8", "success https://example.com/job\n")
        self.assertEqual(self.check("publishers", "0123456789abcdef")[1], "Publishers\tfrom the Release dry run on main today\n")

    def test_a_failed_publishers_job_fails_with_its_address(self):
        self.answer("gh.runs-commit", "7\n")
        self.answer("gh.job-7", "failure https://example.com/job\n")
        status, _, err = self.check("publishers", "0123456789abcdef")
        self.assertEqual(status, 1)
        self.assertIn("https://example.com/job", err)

    def test_no_dry_run_means_not_run_with_the_command_to_start_one(self):
        status, _, err = self.check("publishers", "0123456789abcdef")
        self.assertEqual(status, 2)
        self.assertIn("not run: gh workflow run release.yml -f tag=dry-run", err)


class Served(Tree):
    def test_the_same_installer_passes(self):
        self.assertEqual(self.check("short-installer")[0], 0)

    def test_another_installer_fails(self):
        self.answer("served", "echo old\n")
        status, _, err = self.check("short-installer")
        self.assertEqual(status, 1)
        self.assertIn("copy it to the Pages site", err)

    def test_not_served_yet_passes_with_a_warning(self):
        self.answer("status", "404")
        status, out, err = self.check("short-installer")
        self.assertEqual(status, 0)
        self.assertIn("isn't served yet", out)
        self.assertIn("warning:", err)

    def test_no_network_is_not_run(self):
        self.answer("status", "000")
        self.assertEqual(self.check("short-installer")[0], 2)


class Tools(Tree):
    def test_another_cargo_about_is_not_run(self):
        status, _, err = self.check("notices", FAKE_ABOUT="0.9.1")
        self.assertEqual(status, 2)
        self.assertIn("needs cargo-about 0.9.2", err)
        self.assertIn("this machine has 0.9.1", err)

    def test_notices_that_generate_the_same_pass(self):
        self.assertEqual(self.check("notices")[:2], (0, "THIRD_PARTY_NOTICES.md matches Cargo.lock\tcargo-about 0.9.2, as the workflow pins\n"))

    def test_notices_that_change_fail(self):
        self.write("packaging/notices/generate.sh", 'echo changed > THIRD_PARTY_NOTICES.md\n')
        status, _, err = self.check("notices")
        self.assertEqual(status, 1)
        self.assertIn("THIRD_PARTY_NOTICES.md is out of date", err)

    def test_the_crates_dry_run_counts_them_on_a_fresh_target_that_goes_after(self):
        self.assertEqual(self.check("packages")[:2], (0, "cargo publish --workspace --dry-run --locked\t2 crates, fresh target folder\n"))
        self.assertEqual(list(self.root.glob("release-packages.*")), [])

    def test_the_crates_dry_run_builds_slim(self):
        self.check("packages")
        self.assertEqual((self.fake / "cargo.env").read_text(), "line-tables-only 0\n")

    def test_too_little_room_for_a_fresh_target_is_not_run(self):
        status, _, err = self.check("packages", FAKE_FREE_KB="1048576")
        self.assertEqual(status, 2)
        self.assertIn("has 1 GB free", err)

    def test_cargo_deny_as_ci_pins_it(self):
        self.assertEqual(self.check("licenses")[0], 0)
        self.write(".github/workflows/ci.yml", CI_YML.replace("0.20.2", "0.21.0"))
        status, _, err = self.check("licenses")
        self.assertEqual(status, 2)
        self.assertIn("needs cargo-deny 0.21.0, as CI pins it; this machine has 0.20.2", err)

    def test_an_unknown_check_shows_the_list(self):
        status, _, err = self.check("everything")
        self.assertEqual(status, 2)
        self.assertIn("ci-passed <commit>", err)


class Crates(Tree):
    def test_every_crate_on_crates_io_passes_with_the_count(self):
        self.assertEqual(self.check("crates-exist")[:2], (0, "Every crate is already on crates.io\t2 crates, none new\n"))

    def test_a_crate_crates_io_never_had_fails_naming_it(self):
        self.write("packaging/release/crates.py", crates_py(1))
        status, _, err = self.check("crates-exist")
        self.assertEqual(status, 1)
        self.assertIn("argonauts-world has never been on crates.io", err)
        self.assertIn("error: crates.io hasn't had every crate this tree publishes", err)

    def test_an_index_that_cant_be_read_is_not_run(self):
        self.write("packaging/release/crates.py", crates_py(2))
        status, _, err = self.check("crates-exist")
        self.assertEqual(status, 2)
        self.assertIn("not run: couldn't read crates.io's index", err)


class Rehearsal(Tree):
    def setUp(self):
        super().setUp()
        git = ["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@example.com"]
        subprocess.run(["git", "init", "--quiet", "--initial-branch=main", str(self.repo)], check=True)
        subprocess.run([*git, "add", "-A"], check=True)
        subprocess.run([*git, "commit", "--quiet", "-m", "release"], check=True)
        self.commit = subprocess.run([*git, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()

    def rehearse(self):
        done = subprocess.run(["sh", str(REHEARSE), "v0.2.1"], cwd=self.repo, env=self.env, capture_output=True, text=True)
        return done.returncode, done.stdout

    def test_every_check_held_so_the_pull_request_may_open(self):
        self.answer("gh.runs-commit", "7\n")
        self.answer("gh.job-7", "success https://example.com/job\n")
        status, out = self.rehearse()
        short = self.commit[:9]
        self.assertEqual(status, 0, out)
        lines = out.splitlines()
        self.assertEqual(lines[0], f"Rehearsal for v0.2.1 on {short}, the release pull request's head (every step of release-checks.yml)")
        self.assertEqual(len(lines), 12)
        self.assertTrue(all(line.startswith("✓  ") for line in lines[1:-1]), out)
        self.assertEqual(lines[-1], f"✓ Every check ran on {short}. Open the release pull request.")
        self.assertIn(f"CI passed on {short}", out)
        self.assertIn("from the Release dry run on " + short, out)

    def test_a_check_not_run_holds_back_the_pull_request(self):
        status, out = self.rehearse()
        self.assertEqual(status, 1)
        self.assertIn("·  Publishers", out)
        self.assertIn("not run: gh workflow run release.yml -f tag=dry-run", out)
        self.assertTrue(out.endswith(f"✗ 1 check not run on {self.commit[:9]}, so no release pull request yet.\n"), out)

    def test_crates_are_read_from_the_commit_not_the_tree_it_runs_from(self):
        git = ["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@example.com"]
        self.write("packaging/release/crates.py", crates_py(1))
        subprocess.run([*git, "commit", "--quiet", "-am", "a new crate"], check=True)
        self.write("packaging/release/crates.py", crates_py(0))
        self.answer("gh.runs-commit", "7\n")
        self.answer("gh.job-7", "success https://example.com/job\n")
        status, out = self.rehearse()
        self.assertEqual(status, 1)
        self.assertIn("✗  Every crate is already on crates.io", out)
        self.assertIn("argonauts-world has never been on crates.io", out)

    def test_it_leaves_no_worktree_behind(self):
        self.rehearse()
        listed = subprocess.run(["git", "-C", str(self.repo), "worktree", "list"], capture_output=True, text=True).stdout
        self.assertEqual(len(listed.splitlines()), 1, listed)


if __name__ == "__main__":
    unittest.main()
