//! A check's one-line summary, parsed where a known test runner prints one, such as
//! `41 passed, 1 failed` from cargo, pytest, unittest, node --test, jest, vitest, or go test.

use std::sync::LazyLock;

use regex::Regex;

fn regex(pattern: &str) -> Regex {
    // The patterns are literals, checked by the tests below.
    Regex::new(pattern).unwrap_or_else(|err| panic!("bad summary pattern {pattern}: {err}"))
}

static ANSI: LazyLock<Regex> = LazyLock::new(|| regex(r"\x1b\[[0-9;?]*[ -/]*[@-~]"));
static CARGO_RESULT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored"));
static CARGO_BUILD_FAILED: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^error: could not compile `([^`]+)`"));
static JEST: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^Tests:\s+(.+?),\s+\d+ total\s*$"));
static VITEST: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^\s*Tests\s{2,}(.+?)\s+\(\d+\)\s*$"));
static PYTEST: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?m)^=*\s*((?:\d+ \w+|no tests ran)(?:, \d+ \w+)*) in [\d.]+s(?: \([^)]*\))?\s*=*\s*$")
});
static PYTEST_COUNT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\d+ (?:passed|failed|errors?|skipped)|no tests ran"));
static UNITTEST_RAN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^Ran (\d+) tests? in [\d.]+s\s*$"));
static UNITTEST_END: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^(OK|FAILED)(?: \(([^)]*)\))?\s*$"));
static NODE_TEST: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^(?:#|ℹ) (pass|fail|skipped|todo) (\d+)\s*$"));
static GO_TEST: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^--- (PASS|FAIL): "));
static GO_PACKAGE: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?m)^(ok|FAIL)\s+\S+\s+(?:[\d.]+s|\(cached\)|\[build failed\]|\[setup failed\])")
});

/// `None` when no known runner's summary is in `output`.
pub fn one_line_summary(output: &str) -> Option<String> {
    let text = ANSI.replace_all(output, "");
    cargo(&text)
        .or_else(|| jest(&text))
        .or_else(|| vitest(&text))
        .or_else(|| pytest(&text))
        .or_else(|| unittest(&text))
        .or_else(|| node_test(&text))
        .or_else(|| go_test(&text))
}

fn counts(passed: u64, failed: u64, ignored: u64) -> String {
    let mut line = format!("{passed} passed");
    if failed > 0 {
        line.push_str(&format!(", {failed} failed"));
    }
    if ignored > 0 {
        line.push_str(&format!(", {ignored} ignored"));
    }
    line
}

/// Cargo prints one `test result:` line per test binary, so they add up.
fn cargo(text: &str) -> Option<String> {
    let mut totals: Option<(u64, u64, u64)> = None;
    for found in CARGO_RESULT.captures_iter(text) {
        let number = |i: usize| found[i].parse::<u64>().unwrap_or(0);
        let (p, f, i) = totals.unwrap_or_default();
        totals = Some((p + number(1), f + number(2), i + number(3)));
    }
    if let Some((passed, failed, ignored)) = totals {
        return Some(counts(passed, failed, ignored));
    }
    CARGO_BUILD_FAILED
        .captures(text)
        .map(|found| format!("could not compile {}", &found[1]))
}

fn jest(text: &str) -> Option<String> {
    let found = JEST.captures_iter(text).last()?;
    Some(found[1].trim().to_owned())
}

fn vitest(text: &str) -> Option<String> {
    let found = VITEST.captures_iter(text).last()?;
    Some(
        found[1]
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn pytest(text: &str) -> Option<String> {
    let found = PYTEST
        .captures_iter(text)
        .filter(|found| PYTEST_COUNT.is_match(&found[1]))
        .last()?;
    Some(found[1].to_owned())
}

/// Python's unittest: `Ran 13 tests in 0.002s`, then `OK` or `FAILED (failures=1, errors=1)`,
/// with any skipped ones in the parentheses.
fn unittest(text: &str) -> Option<String> {
    let ran: u64 = UNITTEST_RAN.captures_iter(text).last()?[1].parse().ok()?;
    let end = UNITTEST_END.captures_iter(text).last()?;
    let mut failed = 0;
    let mut skipped = 0;
    for count in end.get(2).map_or("", |found| found.as_str()).split(',') {
        let Some((what, n)) = count.trim().split_once('=') else {
            continue;
        };
        let n: u64 = n.trim().parse().unwrap_or(0);
        match what.trim() {
            "failures" | "errors" | "unexpected successes" => failed += n,
            "skipped" | "expected failures" => skipped += n,
            _ => {}
        }
    }
    let passed = ran.saturating_sub(failed + skipped);
    Some(counts(passed, failed, skipped))
}

/// node --test's closing counts, from the TAP or the spec reporter: `# pass 9`, `ℹ fail 1`.
fn node_test(text: &str) -> Option<String> {
    let (mut passed, mut failed, mut skipped, mut seen) = (0, 0, 0, false);
    for found in NODE_TEST.captures_iter(text) {
        let n: u64 = found[2].parse().unwrap_or(0);
        seen = true;
        match &found[1] {
            "pass" => passed = n,
            "fail" => failed = n,
            _ => skipped += n,
        }
    }
    seen.then(|| counts(passed, failed, skipped))
}

/// Test counts with `-v`, else package counts.
fn go_test(text: &str) -> Option<String> {
    let (mut passed, mut failed) = (0, 0);
    for found in GO_TEST.captures_iter(text) {
        match &found[1] {
            "PASS" => passed += 1,
            _ => failed += 1,
        }
    }
    if passed + failed > 0 {
        return Some(counts(passed, failed, 0));
    }
    let (mut ok, mut failing) = (0, 0);
    for found in GO_PACKAGE.captures_iter(text) {
        match &found[1] {
            "ok" => ok += 1,
            _ => failing += 1,
        }
    }
    match (ok, failing) {
        (0, 0) => None,
        (ok, 0) => Some(format!("{ok} {} passed", packages(ok))),
        (ok, failing) => Some(format!("{ok} {} passed, {failing} failed", packages(ok))),
    }
}

fn packages(n: u64) -> &'static str {
    if n == 1 { "package" } else { "packages" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unittest_and_node_test_say_their_counts() {
        let ok = "............\n------------------------------------------------------------\n\
                  Ran 12 tests in 0.030s\n\nOK\n";
        assert_eq!(one_line_summary(ok).as_deref(), Some("12 passed"));
        let failed = "F.\n----\nRan 13 tests in 0.002s\n\nFAILED (failures=1, skipped=2)\n";
        assert_eq!(
            one_line_summary(failed).as_deref(),
            Some("10 passed, 1 failed, 2 ignored")
        );
        let node = "# tests 9\n# suites 0\n# pass 9\n# fail 0\n# cancelled 0\n# skipped 0\n";
        assert_eq!(one_line_summary(node).as_deref(), Some("9 passed"));
    }

    #[test]
    fn check_summary_parses_known_runners() {
        let cargo = "\
running 3 tests
test a ... ok
test result: FAILED. 40 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s

running 1 test
test result: ok. 1 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.01s
";
        assert_eq!(
            one_line_summary(cargo).as_deref(),
            Some("41 passed, 1 failed, 2 ignored")
        );
        let broken = "error[E0425]: cannot find value `x`\nerror: could not compile `demo` (lib) due to 1 previous error\n";
        assert_eq!(
            one_line_summary(broken).as_deref(),
            Some("could not compile demo")
        );

        let pytest = "collected 42 items\n\nt.py ....F\n\
=================== 1 failed, 41 passed, 2 warnings in 0.12s ===================\n";
        assert_eq!(
            one_line_summary(pytest).as_deref(),
            Some("1 failed, 41 passed, 2 warnings")
        );
        assert_eq!(
            one_line_summary("....\n41 passed in 0.03s\n").as_deref(),
            Some("41 passed")
        );
        assert_eq!(
            one_line_summary("===== no tests ran in 0.01s =====\n").as_deref(),
            Some("no tests ran")
        );

        let jest = "Test Suites: 1 failed, 3 passed, 4 total\n\
Tests:       1 failed, 41 passed, 42 total\nSnapshots:   0 total\nTime:        1.2 s\n";
        assert_eq!(
            one_line_summary(jest).as_deref(),
            Some("1 failed, 41 passed")
        );

        let vitest = " \x1b[2m Test Files \x1b[22m \x1b[1m\x1b[31m1 failed\x1b[39m\x1b[22m | 3 passed (4)\n\
      \x1b[2mTests \x1b[22m \x1b[1m\x1b[31m1 failed\x1b[39m\x1b[22m | \x1b[1m\x1b[32m41 passed\x1b[39m\x1b[22m (42)\n\
   Start at  10:00:00\n";
        assert_eq!(
            one_line_summary(vitest).as_deref(),
            Some("1 failed, 41 passed")
        );

        let go_verbose = "=== RUN   TestA\n--- PASS: TestA (0.00s)\n=== RUN   TestB\n    --- PASS: TestB/sub (0.00s)\n--- FAIL: TestB (0.00s)\nFAIL\nFAIL\texample.com/m\t0.01s\n";
        assert_eq!(
            one_line_summary(go_verbose).as_deref(),
            Some("1 passed, 1 failed")
        );
        let go_packages = "ok  \texample.com/a\t0.01s\nok  \texample.com/b\t(cached)\n?   \texample.com/c\t[no test files]\nFAIL\texample.com/d [build failed]\n";
        assert_eq!(
            one_line_summary(go_packages).as_deref(),
            Some("2 packages passed, 1 failed")
        );

        assert_eq!(one_line_summary("all good\n"), None);
        assert_eq!(one_line_summary(""), None);
    }
}
