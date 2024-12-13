use crate::internal::{CapturedOutput, RegisteredTest, SuiteResult, TestResult};
use crate::output::{LogFile, StdoutOrLogFile, TestRunnerOutput};
use quick_xml::events::Event::Decl;
use quick_xml::events::{BytesCData, BytesDecl};
use quick_xml::Writer;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

pub(crate) struct JUnit {
    writer: Mutex<Writer<StdoutOrLogFile>>,
    show_output: bool,
}

impl JUnit {
    pub fn new(show_output: bool, logfile_path: Option<PathBuf>) -> Self {
        let logfile = logfile_path.map(|path| LogFile::new(path, false));
        let stream = match logfile {
            Some(log) => StdoutOrLogFile::LogFile(log),
            None => StdoutOrLogFile::Stdout(std::io::stdout()),
        };
        let writer = Writer::new_with_indent(stream, b' ', 4);
        Self {
            writer: Mutex::new(writer),
            show_output,
        }
    }

    fn write_system_out<W: Write>(
        &self,
        writer: &mut Writer<W>,
        captured: &[CapturedOutput],
    ) -> Result<(), quick_xml::errors::Error> {
        writer
            .create_element("system-out")
            .write_cdata_content(BytesCData::new(
                captured
                    .iter()
                    .filter_map(|line| match line {
                        CapturedOutput::Stdout { line, .. } => Some(line.clone()),
                        CapturedOutput::Stderr { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))?;
        Ok(())
    }

    fn write_system_err<W: Write>(
        &self,
        writer: &mut Writer<W>,
        captured: &[CapturedOutput],
    ) -> Result<(), quick_xml::errors::Error> {
        writer
            .create_element("system-err")
            .write_cdata_content(BytesCData::new(
                captured
                    .iter()
                    .filter_map(|line| match line {
                        CapturedOutput::Stderr { line, .. } => Some(line.clone()),
                        CapturedOutput::Stdout { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))?;
        Ok(())
    }
}

impl TestRunnerOutput for JUnit {
    fn start_suite(&self, _tests: &[RegisteredTest]) {
        let decl = Decl(BytesDecl::new("1.0", Some("UTF-8"), None));
        self.writer.lock().unwrap().write_event(decl).unwrap();
    }

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
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) {
        let result = SuiteResult::from_test_results(registered_tests, results, exec_time);
        self.writer
            .lock()
            .unwrap()
            .create_element("testsuites")
            .with_attribute(("time", exec_time.as_secs_f64().to_string().as_str()))
            .write_inner_content(|writer| {
                writer
                    .create_element("testsuite")
                    .with_attribute(("name", "test"))
                    .with_attribute(("package", "test1"))
                    .with_attribute(("id", "0"))
                    .with_attribute(("errors", "0"))
                    .with_attribute(("failures", result.failed.to_string().as_str()))
                    .with_attribute(("tests", registered_tests.len().to_string().as_str()))
                    .with_attribute(("skipped", result.ignored.to_string().as_str()))
                    .with_attribute(("time", exec_time.as_secs_f64().to_string().as_str()))
                    .write_inner_content(|writer| {
                        for (test, result) in results {
                            let classname = match result {
                                TestResult::Benchmarked { .. } => {
                                    format!("benchmark::{}", test.crate_and_module())
                                }
                                _ => test.crate_and_module(),
                            };

                            let testcase = writer
                                .create_element("testcase")
                                .with_attribute(("name", test.name.as_str()))
                                .with_attribute(("classname", classname.as_str()));

                            match result {
                                TestResult::Passed {
                                    exec_time,
                                    captured,
                                }
                                | TestResult::Benchmarked {
                                    exec_time,
                                    captured,
                                    ..
                                } => {
                                    if captured.is_empty() || !self.show_output {
                                        testcase
                                            .with_attribute((
                                                "time",
                                                exec_time.as_secs_f64().to_string().as_str(),
                                            ))
                                            .write_empty()?;
                                    } else {
                                        testcase
                                            .with_attribute((
                                                "time",
                                                exec_time.as_secs_f64().to_string().as_str(),
                                            ))
                                            .write_inner_content(|writer| {
                                                self.write_system_out(writer, captured)?;
                                                self.write_system_err(writer, captured)?;
                                                Ok::<(), quick_xml::errors::Error>(())
                                            })?;
                                    }
                                }
                                TestResult::Failed {
                                    exec_time,
                                    captured,
                                    ..
                                } => {
                                    testcase
                                        .with_attribute((
                                            "time",
                                            exec_time.as_secs_f64().to_string().as_str(),
                                        ))
                                        .write_inner_content(|writer| {
                                            let mut failure = writer
                                                .create_element("failure")
                                                .with_attribute(("type", "assert"));

                                            if let Some(message) = result.failure_message() {
                                                failure =
                                                    failure.with_attribute(("message", message));
                                            }

                                            failure.write_empty()?;

                                            if !captured.is_empty() {
                                                self.write_system_out(writer, captured)?;
                                                self.write_system_err(writer, captured)?;
                                            }

                                            Ok::<(), quick_xml::errors::Error>(())
                                        })?;
                                }
                                TestResult::Ignored { .. } => {}
                            };
                        }
                        Ok::<(), quick_xml::errors::Error>(())
                    })?;
                Ok::<(), quick_xml::errors::Error>(())
            })
            .unwrap();
    }

    fn test_list(&self, registered_tests: &[RegisteredTest]) {
        let decl = Decl(BytesDecl::new("1.0", Some("UTF-8"), None));
        let mut writer = self.writer.lock().unwrap();
        writer.write_event(decl).unwrap();
        writer
            .create_element("testsuites")
            .write_inner_content(|writer| {
                writer
                    .create_element("testsuite")
                    .with_attribute(("name", "test"))
                    .with_attribute(("package", "test1"))
                    .with_attribute(("id", "0"))
                    .write_inner_content(|writer| {
                        for test in registered_tests {
                            writer
                                .create_element("testcase")
                                .with_attribute(("name", test.name.as_str()))
                                .with_attribute(("classname", test.crate_and_module().as_str()))
                                .write_empty()?;
                        }
                        Ok::<(), quick_xml::errors::Error>(())
                    })?;
                Ok::<(), quick_xml::errors::Error>(())
            })
            .unwrap();
    }
}
