use anstyle::{AnsiColor, Style};
use std::io::Write;

use crate::args::ColorSetting;
use crate::internal::{RegisteredTest, SuiteResult, TestResult};
use crate::output::TestRunnerOutput;

pub(crate) struct Pretty {
    color: ColorSetting,
    style_ok: Style,
    style_failed: Style,
    style_ignored: Style,
    style_progress: Style,
}

impl Pretty {
    pub fn new(color: ColorSetting) -> Self {
        Self {
            color,
            style_ok: Style::new().fg_color(Some(AnsiColor::Green.into())),
            style_failed: Style::new().bold().fg_color(Some(AnsiColor::Red.into())),
            style_ignored: Style::new().dimmed().fg_color(Some(AnsiColor::Cyan.into())),
            style_progress: Style::new()
                .bold()
                .fg_color(Some(AnsiColor::BrightWhite.into())),
        }
    }
}

impl Write for Pretty {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let stdout = std::io::stdout().lock();
        let mut stdout = anstream::AutoStream::new(
            stdout,
            match self.color {
                ColorSetting::Auto => anstream::ColorChoice::Auto,
                ColorSetting::Always => anstream::ColorChoice::Always,
                ColorSetting::Never => anstream::ColorChoice::Never,
            },
        );
        stdout.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}

impl TestRunnerOutput for Pretty {
    fn start_suite(&mut self, count: usize) {
        writeln!(
            self,
            "{}Running {} tests{}",
            self.style_progress.render(),
            count,
            self.style_progress.render_reset(),
        )
        .unwrap();
        writeln!(self).unwrap();
    }

    fn start_running_test(&mut self, test: &RegisteredTest, idx: usize, count: usize) {
        writeln!(
            self,
            "{}[{}/{}]{} Running test: {}",
            self.style_progress.render(),
            idx + 1,
            count,
            self.style_progress.render_reset(),
            test.fully_qualified_name()
        )
        .unwrap();
    }

    fn finished_running_test(
        &mut self,
        test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    ) {
        let result = match result {
            TestResult::Passed => format!(
                "{}PASSED{}",
                self.style_ok.render(),
                self.style_ok.render_reset()
            ),
            TestResult::Failed { .. } => format!(
                "{}FAILED{}",
                self.style_failed.render(),
                self.style_failed.render_reset()
            ),
            TestResult::Ignored => format!(
                "{}IGNORED{}",
                self.style_ignored.render(),
                self.style_ignored.render_reset()
            ),
        };
        writeln!(
            self,
            "{}[{}/{}]{} Finished test: {} [{result}]",
            self.style_progress.render(),
            idx + 1,
            count,
            self.style_progress.render_reset(),
            test.fully_qualified_name()
        )
        .unwrap();
    }

    fn finished_suite(
        &mut self,
        registered_tests: &[RegisteredTest],
        results: &[(&RegisteredTest, TestResult)],
    ) {
        let result = SuiteResult::from_test_results(registered_tests, results);

        let overall = if result.failed == 0 {
            format!(
                "{}ok{}",
                self.style_ok.render(),
                self.style_ok.render_reset()
            )
        } else {
            format!(
                "{}FAILED{}",
                self.style_failed.render(),
                self.style_failed.render_reset()
            )
        };

        writeln!(self).unwrap();
        writeln!(
            self,
            "test result: {}; {} passed; {} failed; {} ignored; {} filtered out;",
            overall, result.passed, result.failed, result.ignored, result.filtered_out,
        )
        .unwrap();
        writeln!(self).unwrap();
        if result.failed > 0 {
            writeln!(self, "Failed tests:").unwrap();
            for failed in results.iter().filter(|(_, result)| result.is_failed()) {
                writeln!(
                    self,
                    " - {} {}({}){}",
                    failed.0.fully_qualified_name(),
                    self.style_ignored.render(),
                    failed.1.failure_message().unwrap_or("???"),
                    self.style_ignored.render_reset(),
                )
                .unwrap();
            }
            writeln!(self).unwrap();
        }
    }

    fn test_list(&mut self, registered_tests: &[RegisteredTest]) {
        for test in registered_tests {
            writeln!(self, "{}", test.fully_qualified_name()).unwrap();
        }
        writeln!(self).unwrap();
        writeln!(self, "{} tests", registered_tests.len()).unwrap();
    }
}
