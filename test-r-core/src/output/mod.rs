mod ipc;
mod json;
mod junit;
mod pretty;
mod terse;

use crate::args::{Arguments, FormatSetting};
use crate::internal::{RegisteredTest, TestResult};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub trait TestRunnerOutput: Send + Sync {
    fn start_suite(&self, tests: &[RegisteredTest]);
    fn start_running_test(&self, test: &RegisteredTest, idx: usize, count: usize);
    fn finished_running_test(
        &self,
        test: &RegisteredTest,
        idx: usize,
        count: usize,
        result: &TestResult,
    );
    fn finished_suite(
        &self,
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    );
    fn test_list(&self, registered_tests: &[RegisteredTest]);

    fn warning(&self, message: &str) {
        eprintln!("{}", message);
    }
}

pub fn test_runner_output(args: &Arguments) -> Arc<dyn TestRunnerOutput> {
    if args.ipc.is_some() {
        Arc::new(ipc::IpcWorkerOutput::new())
    } else if args.quiet {
        Arc::new(terse::Terse::new())
    } else {
        let logfile = args.logfile.as_ref().map(PathBuf::from);
        match args.format.unwrap_or_default() {
            FormatSetting::Pretty => Arc::new(pretty::Pretty::new(
                args.color.unwrap_or_default(),
                args.show_output,
                logfile,
                args.report_time,
                args.unit_test_threshold(),
                args.integration_test_threshold(),
            )),
            FormatSetting::Terse => Arc::new(terse::Terse::new()),
            FormatSetting::Json => Arc::new(json::Json::new(args.show_output, logfile)),
            FormatSetting::Junit => Arc::new(junit::JUnit::new(args.show_output, logfile)),
        }
    }
}

struct LogFile {
    pub file: std::fs::File,
}

impl LogFile {
    fn new(mut path: PathBuf, merged: bool) -> Self {
        let cwd = std::env::current_dir().unwrap();
        if path.is_relative() {
            path = cwd.join(path);
        }

        if !path.parent().unwrap().exists() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }

        if !merged {
            // Because of https://github.com/rust-lang/rust/issues/105424 we have to generate a unique log file name
            // otherwise the core test runner will overwrite it
            let uuid = uuid::Uuid::new_v4();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let extension = path
                .extension()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            path.set_file_name(format!("{}-{}.{}", stem, uuid, extension));
        }

        eprintln!("Logging to {}", path.to_string_lossy());

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.clone())
            .unwrap_or_else(|_| panic!("Failed to open log file {}", path.to_string_lossy()));
        LogFile { file }
    }
}

enum StdoutOrLogFile {
    Stdout(std::io::Stdout),
    LogFile(LogFile),
}

impl Write for StdoutOrLogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            StdoutOrLogFile::Stdout(stdout) => stdout.write(buf),
            StdoutOrLogFile::LogFile(logfile) => logfile.file.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            StdoutOrLogFile::Stdout(stdout) => stdout.flush(),
            StdoutOrLogFile::LogFile(logfile) => logfile.file.flush(),
        }
    }
}
