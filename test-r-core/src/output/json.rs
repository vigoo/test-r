use crate::internal::{CapturedOutput, RegisteredTest, SuiteResult, TestResult};
use crate::output::{LogFile, StdoutOrLogFile, TestRunnerOutput};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

pub(crate) struct Json {
    show_output: bool,
    target: Mutex<StdoutOrLogFile>,
}

impl Json {
    pub fn new(show_output: bool, logfile_path: Option<PathBuf>) -> Self {
        let target = Mutex::new(match logfile_path {
            Some(path) => StdoutOrLogFile::LogFile(LogFile::new(path, false)),
            None => StdoutOrLogFile::Stdout(std::io::stdout()),
        });
        Self {
            show_output,
            target,
        }
    }
}

impl TestRunnerOutput for Json {
    fn start_suite(&self, count: usize) {
        let mut out = self.target.lock().unwrap();
        writeln!(
            out,
            r#"{{ "type": "suite", "event": "started", "test_count": {count} }}"#
        )
        .expect("Failed to write to output");
    }

    fn start_running_test(&self, test: &RegisteredTest, _idx: usize, _count: usize) {
        let mut out = self.target.lock().unwrap();
        writeln!(
            out,
            r#"{{ "type": "test", "event": "started", "name": "{}" }}"#,
            escape8259::escape(test.fully_qualified_name())
        )
        .expect("Failed to write to output");
    }

    fn finished_running_test(
        &self,
        test: &RegisteredTest,
        _idx: usize,
        _count: usize,
        result: &TestResult,
    ) {
        let mut out = self.target.lock().unwrap();
        let event = match result {
            TestResult::Passed { .. } => Some("ok"),
            TestResult::Failed { .. } => Some("failed"),
            TestResult::Ignored { .. } => Some("ignored"),
            TestResult::Benchmarked { .. } => None,
        };

        if let Some(event) = event {
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
            writeln!(
                out,
                r#"{{ "type": "test", "event": "{event}", "name": "{}"{extra} }}"#,
                escape8259::escape(test.fully_qualified_name())
            )
            .expect("Failed to write to output");
        } else if let TestResult::Benchmarked {
            ns_iter_summ, mb_s, ..
        } = result
        {
            let median = ns_iter_summ.median;
            let deviation = ns_iter_summ.max - ns_iter_summ.min;
            let mbps = if *mb_s == 0 {
                String::new()
            } else {
                format!(r#", "mib_per_second": {}"#, mb_s)
            };

            writeln!(
                out,
                r#"{{ "type": "bench", "name": "{}", "median": {median}, "deviation": {deviation}{mbps} }}"#,
                escape8259::escape(test.fully_qualified_name()),
            ).expect("Failed to write to output");
        }
    }

    fn finished_suite(
        &self,
        registered_tests: &[&RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) {
        let mut out = self.target.lock().unwrap();
        let result = SuiteResult::from_test_results(registered_tests, results, exec_time);
        let event = if result.failed == 0 { "ok" } else { "failed" };
        let passed = result.passed;
        let failed = result.failed;
        let ignored = result.ignored;
        let measured = result.measured;
        let filtered_out = result.filtered_out;
        let exec_time = result.exec_time.as_secs_f64();

        writeln!(out,
            r#"{{ "type": "suite", "event": "{event}", "passed": "{passed}", "failed": {failed}, "ignored": {ignored}, "measured": {measured}, "filtered_out": {filtered_out}, "exec_time": {exec_time} }}"#
        ).expect("Failed to write to output");
    }

    fn test_list(&self, registered_tests: &[&RegisteredTest]) {
        let mut out = self.target.lock().unwrap();
        writeln!(out, r#"["#).expect("Failed to write to output");
        for test in registered_tests {
            writeln!(
                out,
                r#""{}","#,
                escape8259::escape(test.fully_qualified_name())
            )
            .expect("Failed to write to output");
        }
        writeln!(out, r#"]"#).expect("Failed to write to output");
    }
}
