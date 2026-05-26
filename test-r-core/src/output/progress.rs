use crate::internal::{RegisteredTest, TestResult};
use anstyle::{AnsiColor, Style};
use std::io::{IsTerminal, Write};
use std::sync::Mutex;

/// Lightweight progress reporter that writes per-test "Running"/"Finished"
/// lines to stderr. Used by output formats (CTRF, JUnit) whose primary
/// stdout/file output is a machine-readable report so the user still gets
/// real-time feedback about which test is currently executing.
pub(crate) struct StderrProgress {
    state: Mutex<State>,
}

struct State {
    index_field_length: usize,
}

impl StderrProgress {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                index_field_length: 0,
            }),
        }
    }

    fn style_progress() -> Style {
        Style::new()
            .bold()
            .fg_color(Some(AnsiColor::BrightWhite.into()))
    }

    fn style_ok() -> Style {
        Style::new().fg_color(Some(AnsiColor::Green.into()))
    }

    fn style_failed() -> Style {
        Style::new().bold().fg_color(Some(AnsiColor::Red.into()))
    }

    fn style_ignored() -> Style {
        Style::new()
            .dimmed()
            .fg_color(Some(AnsiColor::Yellow.into()))
    }

    fn style_bench() -> Style {
        Style::new().fg_color(Some(AnsiColor::Cyan.into()))
    }

    fn write_line(line: &str) {
        // Only emit ANSI escapes when stderr is a TTY; otherwise plain text.
        let stderr = std::io::stderr();
        let mut stderr = stderr.lock();
        if std::io::stderr().is_terminal() {
            let _ = writeln!(stderr, "{line}");
        } else {
            let _ = writeln!(stderr, "{}", strip_ansi(line));
        }
        let _ = stderr.flush();
    }

    pub(crate) fn start_suite(&self, count: usize) {
        {
            let mut state = self.state.lock().unwrap();
            state.index_field_length = format!("{}/{}", count, count).len();
        }
        let style = Self::style_progress();
        Self::write_line(&format!(
            "{}Running {} tests{}",
            style.render(),
            count,
            style.render_reset(),
        ));
    }

    pub(crate) fn start_running_test(&self, test: &RegisteredTest, idx: usize, count: usize) {
        let padding = self.index_padding(idx, count);
        let style = Self::style_progress();
        Self::write_line(&format!(
            "{}[{}{}/{}]{} Running test: {}",
            style.render(),
            padding,
            idx + 1,
            count,
            style.render_reset(),
            test.fully_qualified_name(),
        ));
    }

    pub(crate) fn finished_running_test(
        &self,
        test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    ) {
        let padding = self.index_padding(idx, count);
        let progress = Self::style_progress();
        let status = match result {
            TestResult::Passed { .. } => {
                let s = Self::style_ok();
                format!("[{}PASSED{}]", s.render(), s.render_reset())
            }
            TestResult::Benchmarked { .. } => {
                let s = Self::style_bench();
                format!("[{}BENCH{}]", s.render(), s.render_reset())
            }
            TestResult::Failed { .. } => {
                let s = Self::style_failed();
                format!("[{}FAILED{}]", s.render(), s.render_reset())
            }
            TestResult::Ignored { .. } => {
                let s = Self::style_ignored();
                format!("[{}IGNORED{}]", s.render(), s.render_reset())
            }
        };

        Self::write_line(&format!(
            "{}[{}{}/{}]{} Finished test: {} {status}",
            progress.render(),
            padding,
            idx + 1,
            count,
            progress.render_reset(),
            test.fully_qualified_name(),
        ));
    }

    fn index_padding(&self, idx: usize, count: usize) -> String {
        let state = self.state.lock().unwrap();
        let index_field = format!("{}/{}", idx + 1, count);
        " ".repeat(state.index_field_length.saturating_sub(index_field.len()))
    }
}

/// Strip ANSI escape sequences (CSI `ESC[...m`) so the progress line stays
/// readable when stderr is redirected to a non-TTY (file, pipe, CI log).
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip until we see a final byte in `@`-`~` range.
            if matches!(chars.next(), Some('[')) {
                for cc in chars.by_ref() {
                    if ('@'..='~').contains(&cc) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
