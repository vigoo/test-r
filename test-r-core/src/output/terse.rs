use crate::args::ColorSetting;
use crate::internal::{RegisteredTest, TestResult};
use crate::output::pretty::Pretty;
use crate::output::TestRunnerOutput;

pub(crate) struct Terse {
    pretty: Pretty,
}

impl Terse {
    pub fn new() -> Self {
        Self {
            pretty: Pretty::new(ColorSetting::default()),
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
            TestResult::Passed => print!("."),
            TestResult::Failed { .. } => print!("F"),
            TestResult::Ignored => print!("i"),
        };
    }

    fn finished_suite(
        &self,
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
    ) {
        self.pretty.finished_suite(registered_tests, results)
    }

    fn test_list(&self, registered_tests: &[RegisteredTest]) {
        self.pretty.test_list(registered_tests)
    }
}
