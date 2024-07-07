use crate::internal::{RegisteredTest, SuiteResult, TestResult};
use crate::output::TestRunnerOutput;

pub(crate) struct Json {}

impl Json {
    pub fn new() -> Self {
        Self {}
    }
}

impl TestRunnerOutput for Json {
    fn start_suite(&self, count: usize) {
        println!(r#"{{ "type": "suite", "event": "started", "test_count": {count} }}"#)
    }

    fn start_running_test(&self, test: &RegisteredTest, _idx: usize, _count: usize) {
        println!(
            r#"{{ "type": "test", "event": "started", "name": "{}" }}"#,
            escape8259::escape(test.fully_qualified_name())
        );
    }

    fn finished_running_test(
        &self,
        test: &RegisteredTest,
        _idx: usize,
        _count: usize,
        result: &TestResult,
    ) {
        let event = match result {
            TestResult::Passed => "ok",
            TestResult::Failed { .. } => "failed",
            TestResult::Ignored => "ignored",
        };
        let extra = match result.failure_message() {
            Some(msg) => format!(r#", "stdout": "Error: \"{}\"\n""#, escape8259::escape(msg)),
            None => "".to_string(),
        };
        println!(
            r#"{{ "type": "test", "event": "{event}", "name": "{}"{extra} }}"#,
            escape8259::escape(test.fully_qualified_name())
        );
    }

    fn finished_suite(
        &self,
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
    ) {
        let result = SuiteResult::from_test_results(registered_tests, results);
        let event = if result.failed == 0 { "ok" } else { "failed" };
        let passed = result.passed;
        let failed = result.failed;
        let ignored = result.ignored;
        let measured = result.measured;
        let filtered_out = result.filtered_out;
        let exec_time = result.exec_time.as_secs_f64();

        println!(
            r#"{{ "type": "suite", "event": "{event}", "passed": "{passed}", "failed": {failed}, "ignored": {ignored}, "measured": {measured}, "filtered_out": {filtered_out}, "exec_time": {exec_time} }}"#
        )
    }

    fn test_list(&self, registered_tests: &[RegisteredTest]) {
        println!(r#"["#);
        for test in registered_tests {
            println!(r#""{}","#, escape8259::escape(test.fully_qualified_name()));
        }
        println!(r#"]"#);
    }
}
