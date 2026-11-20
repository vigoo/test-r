use crate::selection::Selection;
use std::process::{Command, ExitStatus};

#[derive(Debug)]
pub enum BinaryOutcome {
    Passed,
    Failed { code: i32 },
    Crashed { signal: i32 },
    LaunchFailed { error: String },
    // TimedOut and Cancelled will be added with the invocation supervisor.
    NotRun,
}

impl From<ExitStatus> for BinaryOutcome {
    fn from(status: ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return Self::Crashed { signal };
            }
        }
        match status.code() {
            Some(0) => Self::Passed,
            code => Self::Failed {
                code: code.unwrap_or(1),
            },
        }
    }
}

impl BinaryOutcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Passed | Self::NotRun => 0,
            Self::Failed { code } => *code,
            Self::Crashed { signal } => 128 + signal,
            Self::LaunchFailed { .. } => 1,
        }
    }
}

pub struct RunResult {
    /// One outcome per binary, in selection order, including skipped binaries.
    pub outcomes: Vec<BinaryOutcome>,
}

impl RunResult {
    pub fn exit_code(&self) -> i32 {
        // The supervisor will give cancellation and deadline expiry precedence.
        self.outcomes
            .iter()
            .map(BinaryOutcome::exit_code)
            .find(|code| *code != 0)
            .unwrap_or(0)
    }
}

pub fn run(selection: &Selection, args: &[String], no_fail_fast: bool) -> RunResult {
    let mut outcomes = Vec::with_capacity(selection.binaries.len());
    let mut stopped = false;
    for binary in &selection.binaries {
        if stopped {
            outcomes.push(BinaryOutcome::NotRun);
            continue;
        }
        let outcome = match Command::new(&binary.executable)
            .current_dir(&binary.working_directory)
            .args(args)
            .status()
        {
            Ok(status) => BinaryOutcome::from(status),
            Err(error) => BinaryOutcome::LaunchFailed {
                error: error.to_string(),
            },
        };
        let id = &binary.binary_id;
        match &outcome {
            BinaryOutcome::Failed { code } => {
                eprintln!("Test binary {id}: failed with exit code {code}")
            }
            BinaryOutcome::Crashed { signal } => {
                eprintln!("Test binary {id}: crashed with signal {signal}")
            }
            BinaryOutcome::LaunchFailed { error } => eprintln!(
                "Test binary {id}: launch failed ({}): {error}",
                binary.executable
            ),
            BinaryOutcome::Passed | BinaryOutcome::NotRun => {}
        }
        stopped = !matches!(outcome, BinaryOutcome::Passed) && !no_fail_fast;
        outcomes.push(outcome);
    }
    RunResult { outcomes }
}
