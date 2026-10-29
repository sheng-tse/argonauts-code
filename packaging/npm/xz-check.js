// On Linux the npm package unpacks the program from a .tar.xz with the system's tar, and GNU tar needs xz for it.
// Without xz the install would end with only "tar: Error is not recoverable", so this says what to install
// before anything is downloaded. release.yml adds this file to the package dist builds before the release uploads
// it, and calls it first from install.js and from run-argonauts.js. The release archives stay .tar.xz, since every
// self-updater already out there unpacks only those.

const { spawnSync } = require("child_process");
const { readFileSync } = require("fs");

// What to run to get xz, by the distribution /etc/os-release names, or null when it's none of these. A fresh
// Debian or Ubuntu, as a container starts, has no package lists yet, so apt reads them first.
const COMMANDS = [
  [["debian", "ubuntu"], ["apt-get update", "apt-get install xz-utils"]],
  [["amzn"], ["yum install xz"]],
  [["fedora", "rhel", "centos", "rocky", "almalinux"], ["dnf install xz"]],
  [["alpine"], ["apk add xz"]],
  [["arch"], ["pacman -S xz"]],
  [["suse", "opensuse"], ["zypper install xz"]],
];

function installCommand(osRelease, root) {
  const field = (name) => {
    const line = osRelease.split("\n").find((line) => line.startsWith(`${name}=`));
    return line ? line.slice(name.length + 1).replace(/^"|"$/g, "") : "";
  };
  const ids = `${field("ID")} ${field("ID_LIKE")}`.split(/\s+/).filter(Boolean);
  for (const [names, steps] of COMMANDS) {
    if (ids.some((id) => names.includes(id))) {
      return steps.map((step) => (root ? step : `sudo ${step}`)).join(" && ");
    }
  }
  return null;
}

// The words to stop with, or null when the install can go ahead: not on Linux, xz is there, or the system's tar
// unpacks xz itself (BusyBox's and bsdtar do).
function advice({ platform, xz, tar, osRelease, root }) {
  if (platform !== "linux" || xz || /bsdtar|busybox/i.test(tar)) {
    return null;
  }
  const command = installCommand(osRelease, root);
  const how = command
    ? `Install it with \`${command}\`, then try again.`
    : "Install xz with your system's package manager, then try again.";
  return `argonauts-code needs xz to unpack its program, and this system has none.\n${how}`;
}

function said(program, args) {
  const result = spawnSync(program, args, { encoding: "utf8" });
  return {
    ran: !result.error && result.status === 0,
    text: `${result.stdout || ""}${result.stderr || ""}`,
  };
}

function needXz() {
  if (process.platform !== "linux") {
    return;
  }
  let osRelease = "";
  try {
    osRelease = readFileSync("/etc/os-release", "utf8");
  } catch {
    // An unknown distribution gets the general advice.
  }
  const words = advice({
    platform: process.platform,
    xz: said("xz", ["--version"]).ran,
    tar: said("tar", ["--version"]).text,
    osRelease,
    root: typeof process.getuid === "function" && process.getuid() === 0,
  });
  if (words) {
    console.error(words);
    process.exit(1);
  }
}

module.exports = { advice, needXz };
