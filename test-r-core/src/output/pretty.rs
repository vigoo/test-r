use crate::args::ColorSetting;
use crate::internal::{RegisteredTest, SuiteResult, TestResult};
use crate::output::TestRunnerOutput;

pub(crate) struct Pretty {
    colors: ColorSetting,
}

impl Pretty {
    pub fn new(colors: ColorSetting) -> Self {
        Self { colors }
    }
}

impl TestRunnerOutput for Pretty {
    fn start_suite(&mut self, count: usize) {
        println!("Running {} tests", count);
        println!();
    }

    fn start_running_test(&mut self, test: &RegisteredTest, idx: usize, count: usize) {
        println!(
            "[{}/{}] Running test: {}",
            idx + 1,
            count,
            test.fully_qualified_name()
        );
    }

    fn finished_running_test(
        &mut self,
        test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    ) {
        let result = match result {
            TestResult::Passed => "PASSED",
            TestResult::Failed { .. } => "FAILED",
            TestResult::Ignored => "IGNORED",
        };
        println!(
            "[{}/{}] Finished test: {} [{result}]",
            idx + 1,
            count,
            test.fully_qualified_name()
        );
    }

    fn finished_suite(
        &mut self,
        registered_tests: &[RegisteredTest],
        results: &[(&RegisteredTest, TestResult)],
    ) {
        let result = SuiteResult::from_test_results(registered_tests, results);

        let overall = if result.failed == 0 { "ok" } else { "FAILED" };

        println!();
        println!(
            "test result: {}; {} passed; {} failed; {} ignored; {} filtered out;",
            overall, result.passed, result.failed, result.ignored, result.filtered_out
        );
        println!();
        if result.failed > 0 {
            println!("failed tests:");
            for failed in results.iter().filter(|(_, result)| result.is_failed()) {
                println!(
                    " - {} ({})",
                    failed.0.fully_qualified_name(),
                    failed.1.failure_message().unwrap_or("???")
                );
            }
            println!();
        }
    }

    fn test_list(&mut self, registered_tests: &[RegisteredTest]) {
        for test in registered_tests {
            println!("{}", test.fully_qualified_name());
        }
        println!();
        println!("{} tests", registered_tests.len());
    }
}
