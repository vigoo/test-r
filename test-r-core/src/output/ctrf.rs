use crate::internal::{FlakinessControl, RegisteredTest, TestResult};
use crate::output::progress::StderrProgress;
use crate::output::{write_failure_summary_to_stderr, LogFile, StdoutOrLogFile, TestRunnerOutput};
use ctrf_rs::test::{Status, Test};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) struct Ctrf {
    show_output: bool,
    state: Mutex<CtrfState>,
    progress: StderrProgress,
}

impl Ctrf {
    pub fn new(show_output: bool, logfile_path: Option<PathBuf>) -> Self {
        let target = match logfile_path {
            Some(path) => StdoutOrLogFile::LogFile(LogFile::new(path, false)),
            None => StdoutOrLogFile::Stdout(std::io::stdout()),
        };
        Self {
            show_output,
            state: Mutex::new(CtrfState::new(target)),
            progress: StderrProgress::new(),
        }
    }
}

impl TestRunnerOutput for Ctrf {
    fn start_suite(&self, tests: &[RegisteredTest]) {
        let mut state = self.state.lock().unwrap();
        state.start.replace(SystemTime::now());
        self.progress.start_suite(tests.len());
    }

    fn start_running_test(&self, registered_test: &RegisteredTest, idx: usize, count: usize) {
        let mut state = self.state.lock().unwrap();
        let mut test = Test::new(
            registered_test.fully_qualified_name(),
            Status::Pending,
            Duration::ZERO,
        );
        test.start = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        );
        state
            .pending_tests
            .insert(registered_test.fully_qualified_name(), test);
        drop(state);
        self.progress
            .start_running_test(registered_test, idx, count);
    }

    fn repeat_running_test(
        &self,
        registered_test: &RegisteredTest,
        _idx: usize,
        _count: usize,
        _attempt: usize,
        _max_attempts: usize,
        _reason: &str,
    ) {
        let mut state = self.state.lock().unwrap();
        let test = state
            .pending_tests
            .get_mut(&registered_test.fully_qualified_name())
            .expect("repeat_running_test called with a test that has not been started yet");
        test.retries = Some(test.retries.unwrap_or_default() + 1);
        test.flaky = match registered_test.props.flakiness_control {
            FlakinessControl::None => None,
            FlakinessControl::ProveNonFlaky(_) => Some(false),
            FlakinessControl::RetryKnownFlaky(_) => Some(true),
        };
    }

    fn finished_running_test(
        &self,
        registered_test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    ) {
        self.progress
            .finished_running_test(registered_test, idx, count, result);
        let mut state = self.state.lock().unwrap();
        let pending_test = state
            .pending_tests
            .get_mut(&registered_test.fully_qualified_name())
            .expect("finished_running_test called on a test that has not been started before");

        let mut test = Test::new(
            registered_test.fully_qualified_name(),
            match result {
                TestResult::Passed { .. } => Status::Passed,
                TestResult::Benchmarked { .. } => Status::Passed,
                TestResult::Failed { .. } => Status::Failed,
                TestResult::Ignored { .. } => Status::Skipped,
            },
            match result {
                TestResult::Passed { exec_time, .. } => *exec_time,
                TestResult::Failed { exec_time, .. } => *exec_time,
                TestResult::Benchmarked { exec_time, .. } => *exec_time,
                TestResult::Ignored { .. } => Duration::ZERO,
            },
        );

        let mut stdout_lines = vec![];
        let mut stderr_lines = vec![];

        for capture in result.captured_output() {
            match capture {
                crate::internal::CapturedOutput::Stdout { line, .. } => {
                    stdout_lines.push(line.clone())
                }
                crate::internal::CapturedOutput::Stderr { line, .. } => {
                    stderr_lines.push(line.clone())
                }
            }
        }

        if result.is_failed() || self.show_output {
            test.stdout = stdout_lines;
            test.stderr = stderr_lines;
        }

        test.message = result.failure_message();
        test.suite = Some(registered_test.crate_and_module());
        test.flaky = pending_test.flaky;
        test.retries = pending_test.retries;
        test.start = pending_test.start;
        test.stop = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        );

        let status = test.status();
        let suite = test.suite.clone();
        let value = serde_json::to_value(&test).expect("Failed to serialize CTRF test");
        state.tests.push(value);
        match status {
            Status::Passed => state.summary.passed += 1,
            Status::Failed => state.summary.failed += 1,
            Status::Pending => state.summary.pending += 1,
            Status::Skipped => state.summary.skipped += 1,
            Status::Other => state.summary.other += 1,
        }
        if let Some(s) = suite {
            state.suites.insert(s);
        }

        // Write an intermediate snapshot to the log file (if any) so partial
        // results survive even if the process is killed (e.g. due to a hanging
        // test). For stdout we wait for the final report.
        write_ctrf_snapshot(&mut state, false);
    }

    fn finished_suite(
        &self,
        _registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) {
        let mut state = self.state.lock().unwrap();
        write_ctrf_snapshot(&mut state, true);
        // Clear the start marker now that the suite has truly finished.
        state.start = None;

        write_failure_summary_to_stderr(results, exec_time);
    }

    fn test_list(&self, _registered_tests: &[RegisteredTest]) {}
}

/// Serializes the currently accumulated CTRF tests and writes them to the
/// configured output. When the target is a log file the file is truncated
/// first so each snapshot is a complete, valid CTRF document. For stdout the
/// snapshot is only written when `is_final` is true to avoid spamming stdout
/// with partial reports.
///
/// The CTRF JSON document is built manually here (rather than via
/// `ctrf_rs::Report`) so we can keep each previously emitted `Test` as a
/// cached `serde_json::Value`. `ctrf_rs::test::Test` is neither `Clone` nor
/// safely round-trippable through `serde_json` (several of its fields use
/// `skip_serializing_if` without `default`), which would otherwise force us
/// to rebuild every test from scratch on every snapshot.
fn write_ctrf_snapshot(state: &mut CtrfState, is_final: bool) {
    let started = match state.start {
        Some(s) => s,
        None => return,
    };

    let reset = state
        .target
        .reset_log_file()
        .expect("Failed to reset CTRF log file");
    if !reset && !is_final {
        // Writing to stdout - only emit on the final call.
        return;
    }

    let now = SystemTime::now();
    let start_ms = started.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    let stop_ms = now.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    let total = state.summary.passed
        + state.summary.failed
        + state.summary.pending
        + state.summary.skipped
        + state.summary.other;

    let mut summary = json!({
        "tests": total,
        "passed": state.summary.passed,
        "failed": state.summary.failed,
        "pending": state.summary.pending,
        "skipped": state.summary.skipped,
        "other": state.summary.other,
        "start": start_ms,
        "stop": stop_ms,
    });
    if !state.suites.is_empty() {
        summary
            .as_object_mut()
            .unwrap()
            .insert("suites".to_string(), json!(state.suites.len()));
    }

    let report = json!({
        "reportFormat": ctrf_rs::report::REPORT_FORMAT,
        "specVersion": ctrf_rs::report::SPEC_VERSION.to_string(),
        "timestamp": format!("{now:?}"),
        "generatedBy": "test-r",
        "results": {
            "tool": { "name": "ctrf-rs" },
            "summary": summary,
            "tests": state.tests,
        },
    });

    let raw = serde_json::to_string(&report).expect("Failed to serialize CTRF document");
    let out = &mut state.target;
    writeln!(out, "{}", raw).expect("Failed to write to output");
    out.flush().expect("Failed to flush CTRF output");
}

#[derive(Default)]
struct SummaryCounts {
    passed: usize,
    failed: usize,
    pending: usize,
    skipped: usize,
    other: usize,
}

struct CtrfState {
    pub target: StdoutOrLogFile,
    /// Each completed test is cached here as a serialized `serde_json::Value`
    /// because `ctrf_rs::test::Test` is not `Clone` and we need to be able to
    /// rebuild a complete `Results` document each time we emit an intermediate
    /// snapshot.
    pub tests: Vec<serde_json::Value>,
    pub summary: SummaryCounts,
    pub suites: HashSet<String>,
    pub pending_tests: HashMap<String, Test>,
    pub start: Option<SystemTime>,
}

impl CtrfState {
    pub fn new(target: StdoutOrLogFile) -> Self {
        Self {
            target,
            tests: Vec::new(),
            summary: SummaryCounts::default(),
            suites: HashSet::new(),
            pending_tests: HashMap::new(),
            start: None,
        }
    }
}
