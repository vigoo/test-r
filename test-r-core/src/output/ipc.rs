use crate::internal::{RegisteredTest, TestResult};
use crate::output::TestRunnerOutput;
use std::time::Duration;

pub(crate) struct IpcWorkerOutput {}

impl IpcWorkerOutput {
    pub fn new() -> Self {
        Self {}
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
