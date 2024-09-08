use clap::{Parser, ValueEnum};
use std::num::NonZero;

/// Command line arguments.
///
/// This type represents everything the user can specify via CLI args. The main
/// method is [`from_args`][Arguments::from_args] which reads the global
/// `std::env::args()` and parses them into this type.
#[derive(Parser, Debug, Clone, Default)]
#[command(
    help_template = "USAGE: [OPTIONS] [FILTERS...]\n\n{all-args}\n",
    disable_version_flag = true
)]
pub struct Arguments {
    /// Run ignored and not ignored tests
    #[arg(long = "include-ignored")]
    pub include_ignored: bool,

    /// Run only ignored tests
    #[arg(long = "ignored")]
    pub ignored: bool,

    /// Excludes tests marked as should_panic
    #[arg(long = "exclude-should-panic")]
    pub exclude_should_panic: bool,

    /// Run tests and not benchmarks
    #[arg(long = "test", conflicts_with = "bench")]
    pub test: bool,

    /// Run benchmarks instead of tests
    #[arg(long = "bench")]
    pub bench: bool,

    /// List all tests and benchmarks
    #[arg(long = "list")]
    pub list: bool,

    /// Write logs to the specified file
    #[arg(long = "logfile", value_name = "PATH")]
    pub logfile: Option<String>,

    /// don't capture stdout/stderr of each task, allow printing directly
    #[arg(long = "nocapture")]
    pub nocapture: bool,

    /// Number of threads used for running tests in parallel
    #[arg(long = "test-threads")]
    pub test_threads: Option<usize>,

    /// Skip tests whose names contains FILTER (this flag can be used multiple times)
    #[arg(long = "skip", value_name = "FILTER")]
    pub skip: Vec<String>,

    /// Display one character per test instead of one line.
    /// Alias to `--format=terse`
    #[arg(short = 'q', long = "quiet", conflicts_with = "format")]
    pub quiet: bool,

    /// Exactly match filters rather than by substring
    #[arg(long = "exact")]
    pub exact: bool,

    /// Configure coloring of output
    #[arg(long = "color", value_enum, value_name = "auto|always|never")]
    pub color: Option<ColorSetting>,

    /// Configure formatting of output
    #[arg(long = "format", value_enum, value_name = "pretty|terse|json|junit")]
    pub format: Option<FormatSetting>,

    /// Show captured stdout of successful tests
    #[arg(long = "show-output")]
    pub show_output: bool,

    /// Enable nightly-only flags
    #[arg(short = 'Z')]
    pub unstable_flags: Option<UnstableFlags>,

    /// Show execution time of each test.
    /// Threshold values for colorized output can be configured via `RUST_TEST_TIME_UNIT`,
    /// `RUST_TEST_TIME_INTEGRATION` and `RUST_TEST_TIME_DOCTEST` environment variables.
    /// Expected format of the environment variables is `VARIABLE=WARN_TIME,CRITICAL_TIME`.
    /// Durations must be specified in milliseconds, e.g. `500,2000` means that the warn time is 0.5
    /// seconds, and the critical time is 2 seconds.
    /// Not available for `--format=terse`.
    #[arg(long = "report-time")]
    pub report_time: bool,

    /// Treat excess of the test execution time limit as error.
    /// Threshold values for this option can be configured via `RUST_TEST_TIME_UNIT`,
    /// `RUST_TEST_TIME_INTEGRATION` and `RUST_TEST_TIME_DOCTEST` environment variables.
    /// Expected format of the environment variables is `VARIABLE=WARN_TIME,CRITICAL_TIME`.
    /// `CRITICAL_TIME` here means the limit that should not be exceeded by test.
    #[arg(long = "ensure-time")]
    pub ensure_time: bool,

    /// Run tests in random order
    #[arg(long = "shuffle", conflicts_with = "shuffle_seed")]
    pub shuffle: bool,

    /// Run tests in random order; seed the random number generator with SEED
    #[arg(long = "shuffle-seed", value_name = "SEED", conflicts_with = "shuffle")]
    pub shuffle_seed: Option<u64>,

    /// The FILTER string is tested against the name of all tests, and only those
    /// tests whose names contain the filter are run. Multiple filter strings may
    /// be passed, which will run all tests matching any of the filters.
    #[arg(value_name = "FILTER")]
    pub filter: Option<String>,

    /// Run the test suite in worker IPC mode - listening on the given local socket waiting
    /// for the test runner to connect and send test execution requests. The only stdout/stderr
    /// output will be the one emitted by the actual test runs so the test runner can capture them.
    pub ipc: Option<String>,
}

impl Arguments {
    /// Parses the global CLI arguments given to the application.
    ///
    /// If the parsing fails (due to incorrect CLI args), an error is shown and
    /// the application exits. If help is requested (`-h` or `--help`), a help
    /// message is shown and the application exits, too.
    pub fn from_args() -> Self {
        Parser::parse()
    }

    pub(crate) fn test_threads(&self) -> NonZero<usize> {
        if self.ipc.is_some() {
            // When running as an IPC-controlled worker, always use a single thread
            NonZero::new(1).unwrap()
        } else {
            self.test_threads
                .and_then(NonZero::new)
                .or_else(|| std::thread::available_parallelism().ok())
                .unwrap_or(NonZero::new(1).unwrap())
        }
    }
}

impl<A: Into<std::ffi::OsString> + Clone> FromIterator<A> for Arguments {
    fn from_iter<T: IntoIterator<Item = A>>(iter: T) -> Self {
        Parser::parse_from(iter)
    }
}

/// Possible values for the `--color` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ColorSetting {
    /// Colorize if stdout is a tty and tests are run on serially (default)
    #[default]
    Auto,

    /// Always colorize output
    Always,

    /// Never colorize output
    Never,
}

/// Possible values for the `-Z` option
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UnstableFlags {
    /// Allow use of experimental features
    UnstableOptions,
}

/// Possible values for the `--format` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum FormatSetting {
    /// Print verbose output
    #[default]
    Pretty,

    /// Display one character per test
    Terse,

    /// Output a json document
    Json,

    /// Output a JUnit document
    Junit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_cli() {
        use clap::CommandFactory;
        Arguments::command().debug_assert();
    }
}
