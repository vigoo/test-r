use crate::internal::{FlakinessControl, RegisteredTest, TestResult};
use crate::output::{LogFile, StdoutOrLogFile, TestRunnerOutput};
use ctrf_rs::report::Report;
use ctrf_rs::results::ResultsBuilder;
use ctrf_rs::test::{Status, Test};
use ctrf_rs::tool::Tool;
use std::collections::HashMap;
use std::io::Write;
use std::ops::Add;
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

        test.message = result.failure_message().map(|m| m.to_string());
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

        state.builder.as_mut().unwrap().add_test(test);
    }

    fn finished_suite(
        &self,
        _registered_tests: &[RegisteredTest],
        _results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) {
        let mut state = self.state.lock().unwrap();
        let started = state
            .start
            .take()
            .expect("finished_suite called without a start time");
        let results = state
            .builder
            .take()
            .unwrap()
            .build(started, started.add(exec_time));
        let report = Report::new(
            None,
            Some(SystemTime::now()),
            Some("test-r".to_string()),
            results,
        );

        let raw = serde_json::to_string(&report).expect("Failed to serialize CTRF document");
        let out = &mut state.target;
        writeln!(out, "{}", raw).expect("Failed to write to output");
    }

    fn test_list(&self, _registered_tests: &[RegisteredTest]) {}
}

struct CtrfState {
    pub target: StdoutOrLogFile,
    pub builder: Option<ResultsBuilder>,
    pub pending_tests: HashMap<String, Test>,
    pub start: Option<SystemTime>,
}

impl CtrfState {
    pub fn new(target: StdoutOrLogFile) -> Self {
        Self {
            target,
            builder: Some(ResultsBuilder::new(Tool::new(None))),
            pending_tests: HashMap::new(),
            start: None,
        }
    }
}
