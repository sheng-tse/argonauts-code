//! What a launch really runs. `env`, shells, interpreters, and launchers such as `nohup` run
//! a program or script named in their arguments, so every check looks through them.

use super::refusals::program_name;

/// Run a script given as their first argument. Flags before it can run inline code (`sh -c`,
/// `node -e`) or load modules (`python -m`, `node --require`), so none are allowed there.
const SCRIPT_RUNNERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ash",
    "ksh",
    "mksh",
    "yash",
    "fish",
    "csh",
    "tcsh",
    "python",
    "pypy",
    "node",
    "nodejs",
    "deno",
    "bun",
    "perl",
    "ruby",
    "php",
    "lua",
    "luajit",
    "tclsh",
    "expect",
    "rscript",
    "julia",
    "java",
    "osascript",
    "pwsh",
    "powershell",
    "awk",
    "gawk",
    "mawk",
    "nawk",
];

/// Run their first argument that isn't a flag. Some take a value first, such as `timeout 10`,
/// and that value is then checked as the program, which refuses it rather than let it by.
const LAUNCHERS: &[&str] = &[
    "nohup",
    "nice",
    "time",
    "timeout",
    "stdbuf",
    "setsid",
    "caffeinate",
    "arch",
    "exec",
    "command",
    "sudo",
    "doas",
    "pkexec",
    "chrt",
    "taskset",
    "ionice",
    "unbuffer",
    "busybox",
    "xcrun",
    "firejail",
    "flock",
    "chroot",
    "strace",
    "ltrace",
    "valgrind",
];

/// One program a launch runs.
pub(super) struct Step<'a> {
    /// The program that runs this one, or `None` for the launch's own program.
    pub wrapper: Option<&'a str>,
    pub program: &'a str,
    pub args: &'a [String],
}

pub(super) struct Chain<'a> {
    /// Outermost first.
    pub steps: Vec<Step<'a>>,
    /// Why the chain can't be followed further, when a wrapper runs something no check can
    /// see, such as `sh -c` or `env -S`.
    pub refusals: Vec<String>,
}

/// Follows `program` through every wrapper in front of the program that does the work.
pub(super) fn follow<'a>(program: &'a str, args: &'a [String]) -> Chain<'a> {
    let mut chain = Chain {
        steps: vec![Step {
            wrapper: None,
            program,
            args,
        }],
        refusals: Vec::new(),
    };
    let (mut program, mut args) = (program, args);
    loop {
        let name = program_name(program);
        let base = base_name(name);
        let next = if base == "env" {
            env_operand(args)
        } else if SCRIPT_RUNNERS.contains(&base.as_str()) {
            script_operand(name, args)
        } else if LAUNCHERS.contains(&base.as_str()) {
            Ok(first_operand(args))
        } else {
            return chain;
        };
        let index = match next {
            Ok(Some(index)) => index,
            Ok(None) => return chain,
            Err(why) => {
                chain.refusals.push(why);
                return chain;
            }
        };
        let wrapped = args[index].as_str();
        if wrapped.chars().any(char::is_whitespace) {
            chain.refusals.push(format!(
                "{name} runs {wrapped:?}, a command line no check can see into. Name the \
                 program and list its arguments"
            ));
            return chain;
        }
        args = &args[index + 1..];
        program = wrapped;
        chain.steps.push(Step {
            wrapper: Some(name),
            program,
            args,
        });
    }
}

/// `python3.12` and `Node` are `python` and `node`: a version suffix and case don't change
/// what a wrapper does, and macOS file names ignore case.
pub(super) fn base_name(name: &str) -> String {
    name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.')
        .to_ascii_lowercase()
}

/// The index of the program `env` runs. Assignments before it would set variables that
/// `env_set`'s checks never see, and `-S`, `-C`, and `-P` change what runs or where.
fn env_operand(args: &[String]) -> Result<Option<usize>, String> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            return Ok((index + 1 < args.len()).then_some(index + 1));
        }
        if let Some(long) = arg.strip_prefix("--") {
            let flag = long.split_once('=').map_or(long, |(flag, _)| flag);
            if matches!(flag, "split-string" | "chdir") {
                return Err(env_flag_refusal(arg));
            }
            if flag == "unset" && !long.contains('=') {
                index += 1;
            }
        } else if let Some(cluster) = arg.strip_prefix('-') {
            for (at, flag) in cluster.char_indices() {
                match flag {
                    'S' | 'C' | 'P' => return Err(env_flag_refusal(arg)),
                    // The rest of the cluster, or else the next argument, is the name.
                    'u' => {
                        if at + 1 == cluster.len() {
                            index += 1;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        } else if let Some((name, _)) = arg.split_once('=') {
            return Err(format!(
                "env {name}=... sets a variable where no check sees it. Put it in env_set"
            ));
        } else {
            return Ok(Some(index));
        }
        index += 1;
    }
    Ok(None)
}

fn env_flag_refusal(flag: &str) -> String {
    format!("env {flag} changes what runs or where in a way no check can follow")
}

/// The script a shell or interpreter runs, which must come first.
fn script_operand(name: &str, args: &[String]) -> Result<Option<usize>, String> {
    match args.first() {
        None => Err(format!(
            "{name} with no script reads its code from its input"
        )),
        Some(flag) if flag.starts_with('-') => Err(format!(
            "{name} {flag} comes before the script. Name the script first, since a flag there \
             can run inline code or load a module"
        )),
        Some(_) => Ok(Some(0)),
    }
}

fn first_operand(args: &[String]) -> Option<usize> {
    let index = args.iter().position(|arg| !arg.starts_with('-'))?;
    match args.iter().position(|arg| arg == "--") {
        Some(end) if end < index => (end + 1 < args.len()).then_some(end + 1),
        _ => Some(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn programs(program: &str, list: &[&str]) -> (Vec<String>, Vec<String>) {
        let list = args(list);
        let chain = follow(program, &list);
        let programs = chain
            .steps
            .iter()
            .map(|step| step.program.to_owned())
            .collect();
        (programs, chain.refusals)
    }

    #[test]
    fn follows_wrappers_to_the_program() {
        let (steps, refusals) = programs("/usr/bin/env", &["-i", "-u", "X", "npx", "-y", "pkg"]);
        assert_eq!(steps, ["/usr/bin/env", "npx"]);
        assert!(refusals.is_empty());
        let (steps, _) = programs(
            "/usr/bin/nohup",
            &["/usr/bin/env", "--", "/bin/sh", "/opt/run.sh", "acp"],
        );
        assert_eq!(
            steps,
            ["/usr/bin/nohup", "/usr/bin/env", "/bin/sh", "/opt/run.sh"]
        );
        let (steps, _) = programs("/usr/bin/python3.12", &["/opt/agent.py", "-m", "x"]);
        assert_eq!(steps, ["/usr/bin/python3.12", "/opt/agent.py"]);
        let (steps, refusals) = programs("/opt/bin/agent", &["sh", "-c", "x"]);
        assert_eq!(steps, ["/opt/bin/agent"]);
        assert!(refusals.is_empty());
    }

    #[test]
    fn refuses_what_it_cant_follow() {
        let cases: [(&str, &[&str], &str); 9] = [
            ("/bin/sh", &["-c", "npx pkg"], "comes before the script"),
            ("/bin/bash", &["-lc", "x"], "comes before the script"),
            (
                "/usr/bin/node",
                &["--require", "./x.js", "/opt/a.js"],
                "comes before",
            ),
            ("/bin/sh", &[], "reads its code from its input"),
            ("/usr/bin/env", &["-S", "npx -y pkg"], "env -S"),
            ("/usr/bin/env", &["-iS", "npx"], "env -iS"),
            ("/usr/bin/env", &["--chdir=/tmp", "a"], "env --chdir"),
            (
                "/usr/bin/env",
                &["LD_PRELOAD=./x.so", "/opt/a"],
                "env LD_PRELOAD=...",
            ),
            (
                "/usr/bin/nohup",
                &["/usr/local/bin/npx -y pkg"],
                "a command line",
            ),
        ];
        for (program, list, want) in cases {
            let (_, refusals) = programs(program, list);
            assert!(
                refusals.iter().any(|why| why.contains(want)),
                "{program} {list:?}: {refusals:?}"
            );
        }
    }
}
