use crate::args::{ColorSetting, TimeThreshold};
use crate::internal::{RegisteredTest, TestResult};
use crate::output::pretty::Pretty;
use crate::output::TestRunnerOutput;
use std::time::Duration;

pub(crate) struct Terse {
    pretty: Pretty,
}

impl Terse {
    pub fn new() -> Self {
        Self {
            pretty: Pretty::new(
                ColorSetting::default(),
                false,
                None,
                false,
                TimeThreshold::default(),
                TimeThreshold::default(),
            ),
        }
    }
}

impl TestRunnerOutput for Terse {
    fn start_suite(&self, count: usize) {
        self.pretty.start_suite(count)
    }

    fn start_running_test(&self, _test: &RegisteredTest, _idx: usize, _count: usize) {}

    fn finished_running_test(
        &self,
        _test: &RegisteredTest,
        _idx: usize,
        _count: usize,
        result: &TestResult,
    ) {
        match result {
            TestResult::Passed { .. } => print!("."),
            TestResult::Benchmarked { .. } => print!("B"),
            TestResult::Failed { .. } => print!("F"),
            TestResult::Ignored { .. } => print!("i"),
        };
    }

    fn finished_suite(
        &self,
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) {
        self.pretty
            .finished_suite(registered_tests, results, exec_time)
    }

    fn test_list(&self, registered_tests: &[RegisteredTest]) {
        self.pretty.test_list(registered_tests)
    }
}
