//! Parses libtest's human-readable output (stable has no JSON for test results).

use schemars::JsonSchema;
use serde::Serialize;

/// Upper bound on the captured output kept per failed test.
const MAX_FAILURE_OUTPUT: usize = 8 * 1024;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TestResults {
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub failures: Vec<TestFailure>,
}

#[derive(Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct TestFailure {
    pub name: String,
    /// Captured stdout/stderr of the test (panic message, assertion diff…).
    pub output: Option<String>,
}

pub fn parse<S: AsRef<str>>(lines: &[S]) -> TestResults {
    let mut results = TestResults::default();
    // (test name, captured lines) for the `---- name stdout ----` block being read.
    let mut block: Option<(String, Vec<&str>)> = None;

    for line in lines.iter().map(AsRef::as_ref) {
        if let Some(name) = block_header(line) {
            flush(&mut results, block.take());
            block = Some((name.to_owned(), Vec::new()));
        } else if line == "failures:" || line.starts_with("test result:") {
            flush(&mut results, block.take());
        } else if let Some((_, captured)) = &mut block {
            captured.push(line);
        } else if let Some((name, status)) = status_line(line) {
            match status {
                Status::Ok => results.passed += 1,
                Status::Ignored => results.ignored += 1,
                Status::Failed => {
                    results.failed += 1;
                    results.failures.push(TestFailure {
                        name: name.to_owned(),
                        output: None,
                    });
                }
            }
        }
    }
    flush(&mut results, block);
    results
}

enum Status {
    Ok,
    Failed,
    Ignored,
}

/// `test path::to::name ... ok` (also `ignored, <reason>` and doctest names).
fn status_line(line: &str) -> Option<(&str, Status)> {
    let (name, status) = line.strip_prefix("test ")?.rsplit_once(" ... ")?;
    let status = match status {
        "ok" => Status::Ok,
        "FAILED" => Status::Failed,
        s if s == "ignored" || s.starts_with("ignored, ") => Status::Ignored,
        _ => return None,
    };
    Some((name, status))
}

/// `---- path::to::name stdout ----`
fn block_header(line: &str) -> Option<&str> {
    let inner = line.strip_prefix("---- ")?.strip_suffix(" ----")?;
    inner
        .strip_suffix(" stdout")
        .or_else(|| inner.strip_suffix(" stderr"))
}

fn flush(results: &mut TestResults, block: Option<(String, Vec<&str>)>) {
    let Some((name, captured)) = block else {
        return;
    };
    let text = captured.join("\n").trim().to_owned();
    if text.is_empty() {
        return;
    }
    let text = truncate(text);
    match results.failures.iter_mut().find(|f| f.name == name) {
        Some(failure) => match &mut failure.output {
            Some(existing) => {
                existing.push('\n');
                existing.push_str(&text);
            }
            None => failure.output = Some(text),
        },
        // Output for a test whose status line we did not see (e.g. a timeout).
        None => results.failures.push(TestFailure {
            name,
            output: Some(text),
        }),
    }
}

fn truncate(mut text: String) -> String {
    if text.len() > MAX_FAILURE_OUTPUT {
        let mut cut = MAX_FAILURE_OUTPUT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n… (truncated)");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = "\
running 4 tests
test tests::adds ... ok
test tests::slow ... ignored
test tests::later ... ignored, not ready
test tests::breaks ... FAILED

failures:

---- tests::breaks stdout ----

thread 'tests::breaks' panicked at src/lib.rs:12:9:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    tests::breaks

test result: FAILED. 1 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s

running 1 test
test src/lib.rs - add (line 3) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
";

    #[test]
    fn parses_counts_and_failure_output() {
        let lines: Vec<&str> = OUTPUT.lines().collect();
        let results = parse(&lines);
        assert_eq!((results.passed, results.failed, results.ignored), (2, 1, 2));
        assert_eq!(results.failures.len(), 1);
        let failure = &results.failures[0];
        assert_eq!(failure.name, "tests::breaks");
        let output = failure.output.as_deref().unwrap();
        assert!(output.starts_with("thread 'tests::breaks' panicked"));
        assert!(output.contains(" right: 2"));
        assert!(!output.contains("failures:"));
    }

    #[test]
    fn ignores_unrelated_lines() {
        let results = parse(&["test something weird", "testing ... ok", "   Compiling foo"]);
        assert_eq!(results, TestResults::default());
    }
}
