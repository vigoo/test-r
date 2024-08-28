mod json;
mod junit;
mod pretty;
mod terse;

use crate::args::{Arguments, FormatSetting};
use crate::internal::{RegisteredTest, TestResult};
use std::sync::Arc;

pub trait TestRunnerOutput: Send + Sync {
    fn start_suite(&self, count: usize);
    fn start_running_test(&self, test: &RegisteredTest, idx: usize, count: usize);
    fn finished_running_test(
        &self,
        test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    );
    fn finished_suite(
        &self,
        registered_tests: &[&RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
    );
    fn test_list(&self, registered_tests: &[&RegisteredTest]);
}

pub fn test_runner_output(args: &Arguments) -> Arc<dyn TestRunnerOutput> {
    if args.quiet {
        Arc::new(terse::Terse::new())
    } else {
        match args.format.unwrap_or_default() {
            FormatSetting::Pretty => Arc::new(pretty::Pretty::new(args.color.unwrap_or_default())),
            FormatSetting::Terse => Arc::new(terse::Terse::new()),
            FormatSetting::Json => Arc::new(json::Json::new()),
            FormatSetting::Junit => Arc::new(junit::JUnit::new()),
        }
    }
}
