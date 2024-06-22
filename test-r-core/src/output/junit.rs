use crate::internal::{RegisteredTest, SuiteResult, TestResult};
use crate::output::TestRunnerOutput;
use quick_xml::events::BytesDecl;
use quick_xml::events::Event::Decl;
use quick_xml::Writer;
use std::io::Stdout;

pub(crate) struct JUnit {
    writer: Writer<Stdout>,
}

impl JUnit {
    pub fn new() -> Self {
        let stdout = std::io::stdout();
        let writer = Writer::new_with_indent(stdout, b' ', 4);
        Self { writer }
    }
}

impl TestRunnerOutput for JUnit {
    fn start_suite(&mut self, _count: usize) {
        let decl = Decl(BytesDecl::new("1.0", Some("UTF-8"), None));
        self.writer.write_event(decl).unwrap();
    }

    fn start_running_test(&mut self, _test: &RegisteredTest, _idx: usize, _count: usize) {}

    fn finished_running_test(
        &mut self,
        _test: &RegisteredTest,
        _idx: usize,
        _count: usize,
        _result: &TestResult,
    ) {
    }

    fn finished_suite(
        &mut self,
        registered_tests: &[RegisteredTest],
        results: &[(&RegisteredTest, TestResult)],
    ) {
        let result = SuiteResult::from_test_results(registered_tests, results);
        self.writer
            .create_element("testsuites")
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
                    .write_inner_content(|writer| {
                        for (test, result) in results {
                            let testcase = writer
                                .create_element("testcase")
                                .with_attribute(("name", test.name.as_str()))
                                .with_attribute(("classname", test.crate_and_module().as_str()))
                                .with_attribute(("time", "0.0"));

                            match result {
                                TestResult::Passed => {
                                    testcase.write_empty()?;
                                }
                                TestResult::Failed { .. } => {
                                    testcase.write_inner_content(|writer| {
                                        let mut failure = writer
                                            .create_element("failure")
                                            .with_attribute(("type", "assert"));

                                        if let Some(message) = result.failure_message() {
                                            failure = failure.with_attribute(("message", message));
                                        }

                                        failure.write_empty()?;
                                        Ok::<(), quick_xml::errors::Error>(())
                                    })?;
                                }
                                TestResult::Ignored => {}
                            };
                        }
                        Ok::<(), quick_xml::errors::Error>(())
                    })?;
                writer.create_element("system-out").write_empty()?;
                writer.create_element("system-err").write_empty()?;
                Ok::<(), quick_xml::errors::Error>(())
            })
            .unwrap();
    }

    fn test_list(&mut self, registered_tests: &[RegisteredTest]) {
        let decl = Decl(BytesDecl::new("1.0", Some("UTF-8"), None));
        self.writer.write_event(decl).unwrap();
        self.writer
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
