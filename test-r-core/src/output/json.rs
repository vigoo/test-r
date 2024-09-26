use crate::internal::{CapturedOutput, RegisteredTest, SuiteResult, TestResult};
use crate::output::TestRunnerOutput;
use std::time::Duration;

pub(crate) struct Json {
    show_output: bool,
}

impl Json {
    pub fn new(show_output: bool) -> Self {
        Self { show_output }
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
            TestResult::Passed { .. } => "ok",
            TestResult::Failed { .. } => "failed",
            TestResult::Ignored { .. } => "ignored",
        };

        let mut stdout_lines = result
            .captured_output()
            .iter()
            .filter_map(|line| match line {
                CapturedOutput::Stdout { line, .. } => Some(line.clone()),
                CapturedOutput::Stderr { .. } => None,
            })
            .collect::<Vec<_>>();

        let extra = match result.failure_message() {
            Some(msg) => {
                stdout_lines.push(format!("Error: {msg}"));
                let stdout = stdout_lines.join("\n");

                format!(r#", "stdout": "{}"#, escape8259::escape(stdout))
            }
            None => {
                if self.show_output {
                    let stdout = stdout_lines.join("\n");
                    format!(r#", "stdout": "{}"#, escape8259::escape(stdout))
                } else {
                    "".to_string()
                }
            }
        };
        println!(
            r#"{{ "type": "test", "event": "{event}", "name": "{}"{extra} }}"#,
            escape8259::escape(test.fully_qualified_name())
        );
    }

    fn finished_suite(
        &self,
        registered_tests: &[&RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) {
        let result = SuiteResult::from_test_results(registered_tests, results, exec_time);
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

    fn test_list(&self, registered_tests: &[&RegisteredTest]) {
        println!(r#"["#);
        for test in registered_tests {
            println!(r#""{}","#, escape8259::escape(test.fully_qualified_name()));
        }
        println!(r#"]"#);
    }
}
