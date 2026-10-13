use crate::internal::{RegisteredTest, TestResult};
use crate::output::TestRunnerOutput;
use std::time::Duration;

pub(crate) struct IpcWorkerOutput {
    capture_by_default: bool,
}

impl IpcWorkerOutput {
    pub fn new(capture_by_default: bool) -> Self {
        Self { capture_by_default }
    }
}

impl TestRunnerOutput for IpcWorkerOutput {
    fn finish_test_attempt(&self, test: &RegisteredTest) -> Option<String> {
        use std::io::Write;
        if !test
            .props
            .capture_control
            .requires_capturing(self.capture_by_default)
        {
            return None;
        }
        let marker = uuid::Uuid::new_v4().to_string();
        println!("{marker}");
        eprintln!("{marker}");
        std::io::stdout().flush().unwrap();
        std::io::stderr().flush().unwrap();
        Some(marker)
    }

    fn start_suite(&self, _tests: &[RegisteredTest]) {}

    fn start_running_test(&self, _test: &RegisteredTest, _idx: usize, _count: usize) {}

    fn repeat_running_test(
        &self,
        _test: &RegisteredTest,
        _idx: usize,
        _count: usize,
        _attempt: usize,
        _max_attempts: usize,
        _reason: &str,
    ) {
    }

    fn finished_running_test(
        &self,
        _test: &RegisteredTest,
        _idx: usize,
        _count: usize,
        _result: &TestResult,
    ) {
    }

    fn finished_suite(
        &self,
        _registered_tests: &[RegisteredTest],
        _results: &[(RegisteredTest, TestResult)],
        _exec_time: Duration,
    ) {
    }

    fn test_list(&self, _registered_tests: &[RegisteredTest]) {}
}
