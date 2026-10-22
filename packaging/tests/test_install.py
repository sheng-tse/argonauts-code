"""Tests for install.sh, with a fake curl that serves a fake dist installer, so nothing is downloaded. Run from
the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import os
import pathlib
import pty
import subprocess
import tempfile
import textwrap
import unittest

INSTALL_SH = pathlib.Path(__file__).resolve().parent.parent / "install.sh"
RELEASES = "https://github.com/sheng-tse/argonauts-code/releases"

# Answers the URLs install.sh asks for, and notes each one.
CURL = textwrap.dedent("""\
    #!/bin/sh
    for url in "$@"; do :; done
    echo "$url" >> "$WORK/urls"
    case "$url" in
        */argonauts-code-installer.sh) cat "$WORK/installer.sh" ;;
        https://api.github.com/*) printf '[\\n  {\\n    "tag_name": "v0.2.0-beta.1",\\n    "prerelease": true\\n  }\\n]\\n' ;;
        *) exit 22 ;;
    esac
""")

# Does what dist's installer does that install.sh relies on: the binary, and the receipt naming where it went.
INSTALLER = textwrap.dedent("""\
    #!/bin/sh
    echo "$@" > "$WORK/installer-args"
    [ -z "${FAIL:-}" ] || exit 3
    mkdir -p "$HOME/.local/bin" "$HOME/.config/argonauts-code"
    printf '#!/bin/sh\\necho "argonauts $*"\\n' > "$HOME/.local/bin/argonauts"
    chmod +x "$HOME/.local/bin/argonauts"
    printf '{"install_prefix":"%s","version":"0.1.0"}\\n' "$HOME/.local/bin" \\
        > "$HOME/.config/argonauts-code/argonauts-code-receipt.json"
    echo "everything's installed!"
""")


class Install(unittest.TestCase):
    def setUp(self):
        self.work = pathlib.Path(tempfile.mkdtemp())
        bin_dir = self.work / "bin"
        bin_dir.mkdir()
        (bin_dir / "curl").write_text(CURL)
        (bin_dir / "curl").chmod(0o755)
        (self.work / "installer.sh").write_text(INSTALLER)
        self.home = self.work / "home"
        self.home.mkdir()
        self.env = {"PATH": f"{bin_dir}:/usr/bin:/bin", "HOME": str(self.home), "WORK": str(self.work)}

    def install(self, *args, terminal=True, **env):
        """Runs install.sh as `curl … | sh` would, its output on a terminal unless told otherwise."""
        command = ["sh", str(INSTALL_SH), *args]
        environment = {**self.env, **env}
        if not terminal:
            done = subprocess.run(command, env=environment, stdin=subprocess.DEVNULL, capture_output=True, text=True)
            return done.returncode, done.stdout + done.stderr
        screen, terminal = pty.openpty()
        process = subprocess.Popen(command, env=environment, stdin=subprocess.DEVNULL, stdout=terminal,
                                   stderr=terminal)
        os.close(terminal)
        output = b""
        while True:
            try:
                chunk = os.read(screen, 4096)
            except OSError:
                break
            if not chunk:
                break
            output += chunk
        os.close(screen)
        return process.wait(), output.decode()

    def asked_for(self):
        return (self.work / "urls").read_text().split()

    def installer_args(self):
        return (self.work / "installer-args").read_text().split()

    def test_stable_runs_the_latest_releases_installer_then_greets(self):
        code, output = self.install("--no-modify-path")
        self.assertEqual(code, 0, output)
        self.assertEqual(self.asked_for(), [f"{RELEASES}/latest/download/argonauts-code-installer.sh"])
        self.assertEqual(self.installer_args(), ["--no-modify-path"])
        self.assertIn("everything's installed!", output)
        self.assertIn("argonauts welcome", output)
        self.assertLess(output.index("everything's installed!"), output.index("argonauts welcome"))

    def test_beta_runs_the_newest_releases_installer(self):
        code, output = self.install("--beta", "--no-modify-path")
        self.assertEqual(code, 0, output)
        self.assertEqual(
            self.asked_for(),
            [
                "https://api.github.com/repos/sheng-tse/argonauts-code/releases?per_page=1",
                f"{RELEASES}/download/v0.2.0-beta.1/argonauts-code-installer.sh",
            ],
        )
        self.assertEqual(self.installer_args(), ["--no-modify-path"])

    def test_ci_gets_no_greeting(self):
        code, output = self.install(CI="true")
        self.assertEqual(code, 0, output)
        self.assertNotIn("argonauts welcome", output)

    def test_no_greeting_off_a_terminal(self):
        code, output = self.install(terminal=False)
        self.assertEqual(code, 0, output)
        self.assertIn("everything's installed!", output)
        self.assertNotIn("argonauts welcome", output)

    def test_quiet_goes_to_the_installer_and_skips_the_greeting(self):
        code, output = self.install("--quiet")
        self.assertEqual(code, 0, output)
        self.assertEqual(self.installer_args(), ["--quiet"])
        self.assertNotIn("argonauts welcome", output)

    def test_a_failed_install_stops_before_the_greeting(self):
        code, output = self.install(FAIL="1")
        self.assertNotEqual(code, 0)
        self.assertNotIn("argonauts welcome", output)


if __name__ == "__main__":
    unittest.main()
