use crate::args::ColorSetting;
use crate::internal::{RegisteredTest, SuiteResult, TestResult};
use crate::output::TestRunnerOutput;
use anstyle::{AnsiColor, Style};
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

pub(crate) struct Pretty {
    style_ok: Style,
    style_failed: Style,
    style_ignored: Style,
    style_bench: Style,
    style_progress: Style,
    style_stderr: Style,
    lock: Mutex<PrettyImpl>,
    show_output: bool,
}

struct PrettyImpl {
    color: ColorSetting,
}

impl Pretty {
    pub fn new(color: ColorSetting, show_output: bool) -> Self {
        Self {
            style_ok: Style::new().fg_color(Some(AnsiColor::Green.into())),
            style_failed: Style::new().bold().fg_color(Some(AnsiColor::Red.into())),
            style_ignored: Style::new()
                .dimmed()
                .fg_color(Some(AnsiColor::Yellow.into())),
            style_bench: Style::new().dimmed().fg_color(Some(AnsiColor::Cyan.into())),
            style_progress: Style::new()
                .bold()
                .fg_color(Some(AnsiColor::BrightWhite.into())),
            style_stderr: Style::new().fg_color(Some(AnsiColor::Yellow.into())),
            lock: Mutex::new(PrettyImpl { color }),
            show_output,
        }
    }

    fn write_outputs<'a>(
        &self,
        out: &mut PrettyImpl,
        results: impl Iterator<Item = &'a (RegisteredTest, TestResult)>,
    ) {
        for (test, result) in results {
            if !result.captured_output().is_empty() {
                writeln!(out, "---- {} stdout/err ----", test.name).unwrap();
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
}

impl Write for PrettyImpl {
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
    fn start_suite(&self, count: usize) {
        let mut out = self.lock.lock().unwrap();
        writeln!(
            out,
            "{}Running {} tests{}",
            self.style_progress.render(),
            count,
            self.style_progress.render_reset(),
        )
        .unwrap();
        writeln!(out).unwrap();
    }

    fn start_running_test(&self, test: &RegisteredTest, idx: usize, count: usize) {
        let mut out = self.lock.lock().unwrap();
        writeln!(
            out,
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
        &self,
        test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    ) {
        let mut out = self.lock.lock().unwrap();

        let result = match result {
            TestResult::Passed { .. } => format!(
                "{}PASSED{}",
                self.style_ok.render(),
                self.style_ok.render_reset()
            ),
            TestResult::Benchmarked { ns_iter_summ, .. } => format!(
                "{}BENCH          {:>14} ns/iter (+/- {}){}",
                self.style_bench.render(),
                Self::fmt_thousands_sep(ns_iter_summ.median, ','),
                Self::fmt_thousands_sep(ns_iter_summ.max - ns_iter_summ.min, ','),
                self.style_bench.render_reset()
            ),
            TestResult::Failed { .. } => format!(
                "{}FAILED{}",
                self.style_failed.render(),
                self.style_failed.render_reset()
            ),
            TestResult::Ignored { .. } => format!(
                "{}IGNORED{}",
                self.style_ignored.render(),
                self.style_ignored.render_reset()
            ),
        };
        writeln!(
            out,
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
        &self,
        registered_tests: &[&RegisteredTest],
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

    fn test_list(&self, registered_tests: &[&RegisteredTest]) {
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
