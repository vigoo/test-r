use crate::args::{ColorSetting, TimeThreshold};
use crate::internal::{RegisteredTest, SuiteResult, TestResult};
use crate::output::{LogFile, TestRunnerOutput};
use anstyle::{AnsiColor, Style};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub(crate) struct Pretty {
    pub style_ok: Style,
    pub style_failed: Style,
    pub style_ignored: Style,
    pub style_bench: Style,
    style_progress: Style,
    style_stderr: Style,
    style_critical_time: Style,
    style_warn_time: Style,
    lock: Mutex<PrettyImpl>,
    show_output: bool,
    report_time: bool,
    unit_test_threshold: TimeThreshold,
    integ_test_threshold: TimeThreshold,
}

struct PrettyImpl {
    color: ColorSetting,
    logfile: Option<LogFile>,
    pub count: usize,
    pub index_field_length: usize,
    pub longest_name: usize,
}

impl Pretty {
    pub fn new(
        color: ColorSetting,
        show_output: bool,
        logfile_path: Option<PathBuf>,
        report_time: bool,
        unit_test_threshold: TimeThreshold,
        integ_test_threshold: TimeThreshold,
    ) -> Self {
        let logfile = logfile_path.map(|path| LogFile::new(path, false));

        Self {
            style_ok: Style::new().fg_color(Some(AnsiColor::Green.into())),
            style_failed: Style::new().bold().fg_color(Some(AnsiColor::Red.into())),
            style_ignored: Style::new()
                .dimmed()
                .fg_color(Some(AnsiColor::Yellow.into())),
            style_bench: Style::new().fg_color(Some(AnsiColor::Cyan.into())),
            style_progress: Style::new()
                .bold()
                .fg_color(Some(AnsiColor::BrightWhite.into())),
            style_stderr: Style::new().fg_color(Some(AnsiColor::Yellow.into())),
            style_critical_time: Style::new().fg_color(Some(AnsiColor::Red.into())),
            style_warn_time: Style::new().fg_color(Some(AnsiColor::Yellow.into())),
            lock: Mutex::new(PrettyImpl {
                color,
                logfile,
                count: 0,
                longest_name: 0,
                index_field_length: 0,
            }),
            show_output,
            report_time,
            unit_test_threshold,
            integ_test_threshold,
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, impl Write> {
        self.lock.lock().unwrap()
    }

    fn write_outputs<'a>(
        &self,
        out: &mut PrettyImpl,
        results: impl Iterator<Item = &'a (RegisteredTest, TestResult)>,
    ) {
        for (test, result) in results {
            if !result.captured_output().is_empty() {
                writeln!(out, "---- {} stdout/err ----", test.fully_qualified_name()).unwrap();
                for line in result.captured_output() {
                    match line {
                        crate::internal::CapturedOutput::Stdout { line, .. } => {
                            writeln!(out, "{}", line).unwrap();
                        }
                        crate::internal::CapturedOutput::Stderr { line, .. } => {
                            writeln!(
                                out,
                                "{}{}{}",
                                self.style_stderr.render(),
                                line,
                                self.style_stderr.render_reset(),
                            )
                            .unwrap();
                        }
                    }
                }
                writeln!(out).unwrap();
            }
        }
    }

    fn write_success_outputs(
        &self,
        out: &mut PrettyImpl,
        results: &[(RegisteredTest, TestResult)],
    ) {
        self.write_outputs(out, results.iter().filter(|(_, result)| result.is_passed()));
    }

    fn write_failure_outputs(
        &self,
        out: &mut PrettyImpl,
        results: &[(RegisteredTest, TestResult)],
    ) {
        self.write_outputs(out, results.iter().filter(|(_, result)| result.is_failed()));
    }

    // Format a number with thousands separators - from https://github.com/rust-lang/rust/blob/master/library/test/src/bench.rs
    fn fmt_thousands_sep(mut n: f64, sep: char) -> String {
        use std::fmt::Write;
        let mut output = String::new();
        let mut trailing = false;
        for &pow in &[9, 6, 3, 0] {
            let base = 10_usize.pow(pow);
            if pow == 0 || trailing || n / base as f64 >= 1.0 {
                match (pow, trailing) {
                    // modern CPUs can execute multiple instructions per nanosecond
                    // e.g. benching an ADD takes about 0.25ns.
                    (0, true) => write!(output, "{:06.2}", n / base as f64).unwrap(),
                    (0, false) => write!(output, "{:.2}", n / base as f64).unwrap(),
                    (_, true) => write!(output, "{:03}", n as usize / base).unwrap(),
                    _ => write!(output, "{}", n as usize / base).unwrap(),
                }
                if pow != 0 {
                    output.push(sep);
                }
                trailing = true;
            }
            n %= base as f64;
        }

        output
    }

    fn time_style(&self, test_type: &crate::internal::TestType, exec_time: &Duration) -> String {
        let threshold = match test_type {
            crate::internal::TestType::UnitTest => &self.unit_test_threshold,
            crate::internal::TestType::IntegrationTest => &self.integ_test_threshold,
        };
        if threshold.is_critical(exec_time) {
            self.style_critical_time.render().to_string()
        } else if threshold.is_warn(exec_time) {
            self.style_warn_time.render().to_string()
        } else {
            self.style_ok.render().to_string()
        }
    }
}

impl Write for PrettyImpl {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let color_setting = match self.color {
            ColorSetting::Auto => anstream::ColorChoice::Auto,
            ColorSetting::Always => anstream::ColorChoice::Always,
            ColorSetting::Never => anstream::ColorChoice::Never,
        };
        match self.logfile.take() {
            None => {
                let stdout = std::io::stdout().lock();
                let mut stdout = anstream::AutoStream::new(stdout, color_setting);
                stdout.write(buf)
            }
            Some(logfile) => {
                let mut out = anstream::AutoStream::new(logfile.file, color_setting);
                let result = out.write(buf);
                let mut logfile = out.into_inner();
                logfile.flush()?;
                self.logfile = Some(LogFile { file: logfile });
                result
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.logfile {
            None => std::io::stdout().flush(),
            Some(logfile) => logfile.file.flush(),
        }
    }
}

impl TestRunnerOutput for Pretty {
    fn start_suite(&self, tests: &[RegisteredTest]) {
        let mut out = self.lock.lock().unwrap();
        writeln!(
            out,
            "{}Running {} tests{}",
            self.style_progress.render(),
            tests.len(),
            self.style_progress.render_reset(),
        )
        .unwrap();
        writeln!(out).unwrap();

        out.count = tests.len();
        out.longest_name = tests
            .iter()
            .map(|test| test.fully_qualified_name().len())
            .max()
            .unwrap_or(0);
        out.index_field_length = format!("{}/{}", out.count, out.count).len();
    }

    fn start_running_test(&self, test: &RegisteredTest, idx: usize, count: usize) {
        let mut out = self.lock.lock().unwrap();
        let index_field = format!("{}/{}", idx + 1, count);
        let padding = " ".repeat(out.index_field_length - index_field.len());
        writeln!(
            out,
            "{}[{}{}]{} Running test: {}",
            self.style_progress.render(),
            padding,
            index_field,
            self.style_progress.render_reset(),
            test.fully_qualified_name()
        )
        .unwrap();
    }

    fn finished_running_test(
        &self,
        test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    ) {
        let mut out = self.lock.lock().unwrap();

        let result = match result {
            TestResult::Passed { exec_time, .. } => {
                if self.report_time {
                    format!(
                        "[{}PASSED{}]         <{}{:.3}s{}>",
                        self.style_ok.render(),
                        self.style_ok.render_reset(),
                        self.time_style(&test.test_type, exec_time),
                        exec_time.as_secs_f64(),
                        self.style_ok.render_reset()
                    )
                } else {
                    format!(
                        "[{}PASSED{}]",
                        self.style_ok.render(),
                        self.style_ok.render_reset()
                    )
                }
            }
            TestResult::Benchmarked { ns_iter_summ, .. } => format!(
                "[{}BENCH{}]         {}{:>14} ns/iter (+/- {}){}",
                self.style_bench.render(),
                self.style_bench.render_reset(),
                self.style_bench.render(),
                Self::fmt_thousands_sep(ns_iter_summ.median, ','),
                Self::fmt_thousands_sep(ns_iter_summ.max - ns_iter_summ.min, ','),
                self.style_bench.render_reset()
            ),
            TestResult::Failed { exec_time, .. } => {
                if self.report_time {
                    format!(
                        "[{}FAILED{}]         <{}{:.3}s{}>",
                        self.style_failed.render(),
                        self.style_failed.render_reset(),
                        self.time_style(&test.test_type, exec_time),
                        exec_time.as_secs_f64(),
                        self.style_ok.render_reset()
                    )
                } else {
                    format!(
                        "[{}FAILED{}]",
                        self.style_failed.render(),
                        self.style_failed.render_reset()
                    )
                }
            }
            TestResult::Ignored { .. } => format!(
                "[{}IGNORED{}]",
                self.style_ignored.render(),
                self.style_ignored.render_reset()
            ),
        };

        let index_field = format!("{}/{}", idx + 1, count);
        let padding = " ".repeat(out.index_field_length - index_field.len());
        let result_padding = " ".repeat(out.longest_name - test.fully_qualified_name().len() + 1);

        writeln!(
            out,
            "{}[{}{}]{} Finished test: {}{result_padding}{result}",
            self.style_progress.render(),
            padding,
            index_field,
            self.style_progress.render_reset(),
            test.fully_qualified_name()
        )
        .unwrap();
    }

    fn finished_suite(
        &self,
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) {
        let mut out = self.lock.lock().unwrap();

        let result = SuiteResult::from_test_results(registered_tests, results, exec_time);

        if self.show_output {
            self.write_success_outputs(&mut out, results);
        }
        self.write_failure_outputs(&mut out, results);

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

        writeln!(out).unwrap();
        writeln!(
            out,
            "test result: {}; {} passed; {} failed; {} ignored; {} measured; {} filtered out; finished in {:.3}s",
            overall, result.passed, result.failed, result.ignored, result.measured, result.filtered_out, result.exec_time.as_secs_f64()
        )
        .unwrap();
        writeln!(out).unwrap();
        if result.failed > 0 {
            writeln!(out, "Failed tests:").unwrap();
            for failed in results.iter().filter(|(_, result)| result.is_failed()) {
                writeln!(
                    out,
                    " - {} {}({}){}",
                    failed.0.fully_qualified_name(),
                    self.style_ignored.render(),
                    failed.1.failure_message().unwrap_or("???"),
                    self.style_ignored.render_reset(),
                )
                .unwrap();
            }
            writeln!(out).unwrap();
        }
    }

    fn test_list(&self, registered_tests: &[RegisteredTest]) {
        let mut out = self.lock.lock().unwrap();

        for test in registered_tests {
            writeln!(out, "{}", test.fully_qualified_name()).unwrap();
        }
        writeln!(out).unwrap();
        writeln!(out, "{} tests", registered_tests.len()).unwrap();
    }

    fn warning(&self, message: &str) {
        eprintln!(
            "{}{}{}",
            self.style_stderr.render(),
            message,
            self.style_stderr.render_reset()
        );
    }
}
