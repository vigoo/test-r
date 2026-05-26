use crate::internal::{FlakinessControl, RegisteredTest, TestResult};
use crate::output::{write_failure_summary_to_stderr, LogFile, StdoutOrLogFile, TestRunnerOutput};
use ctrf_rs::report::Report;
use ctrf_rs::results::ResultsBuilder;
use ctrf_rs::test::{Status, Test};
use ctrf_rs::tool::Tool;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) struct Ctrf {
    show_output: bool,
    state: Mutex<CtrfState>,
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
        }
    }
}

impl TestRunnerOutput for Ctrf {
    fn start_suite(&self, _tests: &[RegisteredTest]) {
        let mut state = self.state.lock().unwrap();
        state.start.replace(SystemTime::now());
    }

    fn start_running_test(&self, registered_test: &RegisteredTest, _idx: usize, _count: usize) {
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
        _idx: usize,
        _count: usize,
        result: &TestResult,
    ) {
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

        let value = serde_json::to_value(&test).expect("Failed to serialize CTRF test");
        state.tests.push(value);

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

    let mut builder = ResultsBuilder::new(Tool::new(None));
    for value in &state.tests {
        let test: Test =
            serde_json::from_value(value.clone()).expect("Failed to deserialize cached CTRF test");
        builder.add_test(test);
    }
    let ctrf_results = builder.build(started, SystemTime::now());
    let report = Report::new(
        None,
        Some(SystemTime::now()),
        Some("test-r".to_string()),
        ctrf_results,
    );

    let raw = serde_json::to_string(&report).expect("Failed to serialize CTRF document");
    let out = &mut state.target;
    writeln!(out, "{}", raw).expect("Failed to write to output");
    out.flush().expect("Failed to flush CTRF output");
}

struct CtrfState {
    pub target: StdoutOrLogFile,
    /// Each completed test is cached here as a serialized `serde_json::Value`
    /// because `ctrf_rs::test::Test` is not `Clone` and we need to be able to
    /// rebuild a complete `Results` document each time we emit an intermediate
    /// snapshot.
    pub tests: Vec<serde_json::Value>,
    pub pending_tests: HashMap<String, Test>,
    pub start: Option<SystemTime>,
}

impl CtrfState {
    pub fn new(target: StdoutOrLogFile) -> Self {
        Self {
            target,
            tests: Vec::new(),
            pending_tests: HashMap::new(),
            start: None,
        }
    }
}
