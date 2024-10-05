use crate::execution::TestSuiteExecution;
use crate::output::TestRunnerOutput;
use clap::{Parser, ValueEnum};
use std::ffi::OsString;
use std::num::NonZero;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

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
    #[arg(long = "ipc", hide = true)]
    pub ipc: Option<String>,

    /// If true, spawn worker processes in IPC mode and run the tests on those
    #[arg(long = "spawn-workers", hide = true)]
    pub spawn_workers: bool,
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

    /// Renders the arguments as a list of strings that can be passed to a subprocess
    pub fn to_args(&self) -> Vec<OsString> {
        let mut result = Vec::new();

        if self.include_ignored {
            result.push(OsString::from("--include-ignored"));
        }

        if self.ignored {
            result.push(OsString::from("--ignored"));
        }

        if self.exclude_should_panic {
            result.push(OsString::from("--exclude-should-panic"));
        }

        if self.test {
            result.push(OsString::from("--test"));
        }

        if self.bench {
            result.push(OsString::from("--bench"));
        }

        if self.list {
            result.push(OsString::from("--list"));
        }

        if let Some(logfile) = &self.logfile {
            result.push(OsString::from("--logfile"));
            result.push(OsString::from(logfile));
        }

        if self.nocapture {
            result.push(OsString::from("--nocapture"));
        }

        if let Some(test_threads) = self.test_threads {
            result.push(OsString::from("--test-threads"));
            result.push(OsString::from(test_threads.to_string()));
        }

        for skip in &self.skip {
            result.push(OsString::from("--skip"));
            result.push(OsString::from(skip));
        }

        if self.quiet {
            result.push(OsString::from("--quiet"));
        }

        if self.exact {
            result.push(OsString::from("--exact"));
        }

        if let Some(color) = self.color {
            result.push(OsString::from("--color"));
            match color {
                ColorSetting::Auto => result.push(OsString::from("auto")),
                ColorSetting::Always => result.push(OsString::from("always")),
                ColorSetting::Never => result.push(OsString::from("never")),
            }
        }

        if let Some(format) = self.format {
            result.push(OsString::from("--format"));
            match format {
                FormatSetting::Pretty => result.push(OsString::from("pretty")),
                FormatSetting::Terse => result.push(OsString::from("terse")),
                FormatSetting::Json => result.push(OsString::from("json")),
                FormatSetting::Junit => result.push(OsString::from("junit")),
            }
        }

        if self.show_output {
            result.push(OsString::from("--show-output"));
        }

        if let Some(unstable_flags) = &self.unstable_flags {
            result.push(OsString::from("-Z"));
            match unstable_flags {
                UnstableFlags::UnstableOptions => result.push(OsString::from("unstable-options")),
            }
        }

        if self.report_time {
            result.push(OsString::from("--report-time"));
        }

        if self.ensure_time {
            result.push(OsString::from("--ensure-time"));
        }

        if self.shuffle {
            result.push(OsString::from("--shuffle"));
        }

        if let Some(shuffle_seed) = &self.shuffle_seed {
            result.push(OsString::from("--shuffle-seed"));
            result.push(OsString::from(shuffle_seed.to_string()));
        }

        if let Some(filter) = &self.filter {
            result.push(OsString::from(filter));
        }

        if let Some(ipc) = &self.ipc {
            result.push(OsString::from("--ipc"));
            result.push(OsString::from(ipc));
        }

        if self.spawn_workers {
            result.push(OsString::from("--spawn-workers"));
        }

        result
    }

    pub fn unit_test_threshold(&self) -> TimeThreshold {
        TimeThreshold::from_env_var("RUST_TEST_TIME_UNIT").unwrap_or(TimeThreshold::new(
            Duration::from_millis(50),
            Duration::from_millis(100),
        ))
    }

    pub fn integration_test_threshold(&self) -> TimeThreshold {
        TimeThreshold::from_env_var("RUST_TEST_TIME_INTEGRATION").unwrap_or(TimeThreshold::new(
            Duration::from_millis(500),
            Duration::from_millis(1000),
        ))
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

    /// Make necessary adjustments to the configuration if needed based on the final execution plan
    pub(crate) fn finalize_for_execution(
        &mut self,
        execution: &TestSuiteExecution,
        output: Arc<dyn TestRunnerOutput>,
    ) {
        if self.nocapture || self.ipc.is_some() {
            // If there is no need to capture the output, there are no restrictions to check and apply
            // If this is an IPC worker, we don't need to do anything either, as the top level test runner already sets the proper arguments
        } else {
            // If capture is enabled, we need to spawn at least one worker process
            self.spawn_workers = true;

            if self.test_threads().get() > 1 {
                // If tests are executed in parallel, and output needs to be captured, there cannot be any dependencies
                // because it can only be done through spawned workers

                if execution.has_dependencies() {
                    output.warning("Cannot run tests in parallel when test have shared dependencies and output capturing is on. Using a single thread.");
                    self.test_threads = Some(1); // Falling back to single-threaded execution
                }
            }
        }
    }
}

impl<A: Into<OsString> + Clone> FromIterator<A> for Arguments {
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

/// Structure denoting time limits for test execution.
///
/// From https://github.com/rust-lang/rust/blob/master/library/test/src/time.rs
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeThreshold {
    pub warn: Duration,
    pub critical: Duration,
}

impl TimeThreshold {
    /// Creates a new `TimeThreshold` instance with provided durations.
    pub fn new(warn: Duration, critical: Duration) -> Self {
        Self { warn, critical }
    }

    /// Attempts to create a `TimeThreshold` instance with values obtained
    /// from the environment variable, and returns `None` if the variable
    /// is not set.
    /// Environment variable format is expected to match `\d+,\d+`.
    ///
    /// # Panics
    ///
    /// Panics if variable with provided name is set but contains inappropriate
    /// value.
    pub fn from_env_var(env_var_name: &str) -> Option<Self> {
        let durations_str = std::env::var(env_var_name).ok()?;
        let (warn_str, critical_str) = durations_str.split_once(',').unwrap_or_else(|| {
            panic!(
                "Duration variable {env_var_name} expected to have 2 numbers separated by comma, but got {durations_str}"
            )
        });

        let parse_u64 = |v| {
            u64::from_str(v).unwrap_or_else(|_| {
                panic!(
                    "Duration value in variable {env_var_name} is expected to be a number, but got {v}"
                )
            })
        };

        let warn = parse_u64(warn_str);
        let critical = parse_u64(critical_str);
        if warn > critical {
            panic!("Test execution warn time should be less or equal to the critical time");
        }

        Some(Self::new(
            Duration::from_millis(warn),
            Duration::from_millis(critical),
        ))
    }

    pub fn is_critical(&self, duration: &Duration) -> bool {
        *duration >= self.critical
    }

    pub fn is_warn(&self, duration: &Duration) -> bool {
        *duration >= self.warn
    }
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
