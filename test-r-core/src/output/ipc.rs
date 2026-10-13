use crate::internal::{RegisteredTest, TestResult};
use crate::ipc::TestRetry;
use crate::output::TestRunnerOutput;
use parking_lot::Mutex;
use std::time::Duration;

pub(crate) struct IpcWorkerOutput {
    retries: Mutex<Vec<TestRetry>>,
}

impl IpcWorkerOutput {
    pub fn new() -> Self {
        Self {
            retries: Mutex::new(Vec::new()),
        }
    }

    pub fn take_retries(&self) -> Vec<TestRetry> {
        std::mem::take(&mut *self.retries.lock())
    }
}

impl TestRunnerOutput for IpcWorkerOutput {
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
