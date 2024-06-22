mod json;
mod junit;
mod pretty;
mod terse;

use crate::args::{Arguments, FormatSetting};
use crate::internal::{RegisteredTest, TestResult};

pub trait TestRunnerOutput {
    fn start_suite(&mut self, count: usize);
    fn start_running_test(&mut self, test: &RegisteredTest, idx: usize, count: usize);
    fn finished_running_test(
        &mut self,
        test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    );
    fn finished_suite(
        &mut self,
        registered_tests: &[RegisteredTest],
        results: &[(&RegisteredTest, TestResult)],
    );
    fn test_list(&mut self, registered_tests: &[RegisteredTest]);
}

pub fn test_runner_output(args: &Arguments) -> Box<dyn TestRunnerOutput> {
    match args.format.unwrap_or_default() {
        FormatSetting::Pretty => Box::new(pretty::Pretty::new(args.color.unwrap_or_default())),
        FormatSetting::Terse => Box::new(terse::Terse::new()),
        FormatSetting::Json => Box::new(json::Json::new()),
        FormatSetting::Junit => Box::new(junit::JUnit::new()),
    }
}
