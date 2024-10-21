use crate::args::{ColorSetting, TimeThreshold};
use crate::internal::{RegisteredTest, TestResult};
use crate::output::pretty::Pretty;
use crate::output::TestRunnerOutput;
use anstyle::Style;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) struct Terse {
    pretty: Pretty,
    state: Arc<Mutex<TerseOutputState>>,
}

impl Terse {
    const QUIET_MODE_MAX_COLUMN: usize = 80;

    pub fn new() -> Self {
        Self {
            pretty: Pretty::new(
                ColorSetting::default(),
                false,
                None,
                false,
                TimeThreshold::default(),
                TimeThreshold::default(),
            ),
            state: Arc::new(Mutex::new(TerseOutputState::default())),
        }
    }
}

impl TestRunnerOutput for Terse {
    fn start_suite(&self, count: usize) {
        self.pretty.start_suite(count)
    }

    fn start_running_test(&self, _test: &RegisteredTest, _idx: usize, _count: usize) {}

    fn finished_running_test(
        &self,
        test: &RegisteredTest,
        _idx: usize,
        _count: usize,
        result: &TestResult,
    ) {
        let mut state = self.state.lock().unwrap();
        let mut out = self.pretty.lock();
        match result {
            TestResult::Passed { .. } => state.print_char(&mut *out, &self.pretty.style_ok, '.'),
            TestResult::Benchmarked { .. } => state.print_inline(
                &mut *out,
                format!(
                    "{}bench{}",
                    self.pretty.style_bench.render(),
                    self.pretty.style_bench.render_reset()
                )
                .as_str(),
            ),
            TestResult::Failed { .. } => state.print_separate_line(
                &mut *out,
                &format!(
                    "{} --- {}FAILED{}",
                    test.fully_qualified_name(),
                    self.pretty.style_failed.render(),
                    self.pretty.style_failed.render_reset()
                ),
            ),
            TestResult::Ignored { .. } => {
                state.print_char(&mut *out, &self.pretty.style_ignored, '.')
            }
        };
    }

    fn finished_suite(
        &self,
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) {
        let mut state = self.state.lock().unwrap();
        let mut out = self.pretty.lock();
        state.print_separate_line(&mut *out, "");
        drop(out);

        self.pretty
            .finished_suite(registered_tests, results, exec_time)
    }

    fn test_list(&self, registered_tests: &[RegisteredTest]) {
        self.pretty.test_list(registered_tests)
    }
}

#[derive(Debug, Default)]
struct TerseOutputState {
    pub column: usize,
}

impl TerseOutputState {
    pub fn print_char(&mut self, out: &mut impl Write, style: &Style, c: char) {
        write!(out, "{}{}{}", style.render(), c, style.render_reset()).unwrap();
        out.flush().unwrap();
        self.column += 1;
        if self.column > Terse::QUIET_MODE_MAX_COLUMN {
            println!();
            self.column = 0;
        }
    }

    pub fn print_inline(&mut self, out: &mut impl Write, str: &str) {
        write!(out, "{}", str).unwrap();
        out.flush().unwrap();
        self.column += str.len();
    }

    pub fn print_separate_line(&mut self, out: &mut impl Write, line: &str) {
        if self.column > 0 {
            writeln!(out).unwrap();
        }
        self.column = 0;
        writeln!(out, "{}", line).unwrap();
    }
}
