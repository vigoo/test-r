use crate::selection::Selection;
use crate::supervise::Supervisor;
use std::process::{Command, ExitStatus};

#[derive(Debug)]
pub enum BinaryOutcome {
    Passed,
    Failed {
        code: i32,
    },
    #[cfg_attr(not(unix), allow(dead_code))]
    Crashed {
        signal: i32,
    },
    LaunchFailed {
        error: String,
    },
    TimedOut,
    Cancelled {
        signal: i32,
    },
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
    pub fn name(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed { .. } => "failed",
            Self::Crashed { .. } => "crashed",
            Self::LaunchFailed { .. } => "launch-failed",
            Self::TimedOut => "timed-out",
            Self::Cancelled { .. } => "cancelled",
            Self::NotRun => "not-run",
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Passed | Self::NotRun => 0,
            Self::Failed { code } => *code,
            Self::Crashed { signal } => 128 + signal,
            Self::LaunchFailed { .. } => 1,
            Self::TimedOut => 124,
            Self::Cancelled { signal } => 128 + signal,
        }
    }
}

pub struct RunResult {
    /// One outcome per binary, in selection order, including skipped binaries.
    pub outcomes: Vec<BinaryOutcome>,
    pub cleanup_failed: bool,
    pub stop_reason: Option<BinaryOutcome>,
}

impl RunResult {
    pub fn exit_code(&self) -> i32 {
        if let Some(outcome) = self
            .outcomes
            .iter()
            .chain(self.stop_reason.iter())
            .find(|o| matches!(o, BinaryOutcome::Cancelled { .. }))
        {
            return outcome.exit_code();
        }
        if self
            .outcomes
            .iter()
            .chain(self.stop_reason.iter())
            .any(|o| matches!(o, BinaryOutcome::TimedOut))
        {
            return 124;
        }
        self.outcomes
            .iter()
            .map(BinaryOutcome::exit_code)
            .find(|code| *code != 0)
            .unwrap_or(i32::from(self.cleanup_failed))
    }
}

pub fn run(
    selection: &Selection,
    args: &[String],
    no_fail_fast: bool,
    cleanup: bool,
    supervisor: &mut Supervisor,
    events: &mut crate::events::Events,
) -> RunResult {
    let mut outcomes = Vec::with_capacity(selection.binaries.len());
    let mut stopped = false;
    for (index, binary) in selection.binaries.iter().enumerate() {
        if stopped || supervisor.stop_reason().is_some() {
            outcomes.push(BinaryOutcome::NotRun);
            continue;
        }
        let mut observer = events.binary(binary, args, index);
        let mut command = Command::new(&binary.executable);
        command.current_dir(&binary.working_directory).args(args);
        observer.configure(&mut command);
        let outcome = supervisor.run(&mut command, cleanup, Some(&mut observer));
        observer.finished(&outcome);
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
            BinaryOutcome::TimedOut => eprintln!("Invocation deadline expired at {id}"),
            BinaryOutcome::Cancelled { signal } => {
                eprintln!("Invocation cancelled by signal {signal} at {id}")
            }
        }
        stopped = matches!(
            outcome,
            BinaryOutcome::TimedOut | BinaryOutcome::Cancelled { .. }
        ) || (!matches!(outcome, BinaryOutcome::Passed) && !no_fail_fast)
            || supervisor.cleanup_failed;
        outcomes.push(outcome);
    }
    RunResult {
        outcomes,
        stop_reason: supervisor.stop_if_requested(),
        cleanup_failed: supervisor.cleanup_failed,
    }
}

/// Remove only our flags, leaving Cargo and harness arguments unchanged.
pub fn cargo_args(args: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            result.push(arg.clone());
            result.extend(args.cloned());
            break;
        }
        if arg == "--deadline" || arg == "--kill-grace" || arg == "--invocation-id" {
            args.next();
        } else if arg != "--keep-leftovers"
            && !arg.starts_with("--deadline=")
            && !arg.starts_with("--kill-grace=")
            && !arg.starts_with("--invocation-id=")
        {
            result.push(arg.clone());
        }
    }
    result
}
