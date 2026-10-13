use crate::internal::{RegisteredTest, TestResult};
use crate::ipc::TestRetry;
use crate::output::TestRunnerOutput;
use parking_lot::Mutex;
use std::time::Duration;

pub(crate) struct IpcWorkerOutput {
    capture_by_default: bool,
    retries: Mutex<Vec<TestRetry>>,
}

impl IpcWorkerOutput {
    pub fn new(capture_by_default: bool) -> Self {
        Self {
            capture_by_default,
            retries: Mutex::new(Vec::new()),
        }
    }

    pub fn take_retries(&self) -> Vec<TestRetry> {
        std::mem::take(&mut *self.retries.lock())
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
        attempt: usize,
        max_attempts: usize,
        reason: &str,
    ) {
        self.retries.lock().push(TestRetry {
            attempt,
            max_attempts,
            reason: reason.to_string(),
        });
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
