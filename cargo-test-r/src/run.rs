use crate::selection::Selection;
use crate::supervise::Supervisor;
use std::process::{Command, ExitStatus};
use std::time::Duration;

/// How a prebuilt run admits binaries and cleans up after them.
pub struct Options {
    pub no_fail_fast: bool,
    pub deadline: Option<Duration>,
    pub kill_grace: Duration,
    pub keep_leftovers: bool,
}

/// Why the supervisor stopped admitting and running work. Both reasons are
/// sticky: once observed, every later check reports one of them, and a signal
/// replaces an earlier deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Deadline,
    Signal(i32),
}

impl From<StopReason> for BinaryOutcome {
    fn from(reason: StopReason) -> Self {
        match reason {
            StopReason::Deadline => Self::TimedOut,
            StopReason::Signal(signal) => Self::Cancelled { signal },
        }
    }
}

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
    /// Observed after the last binary; covers every binary that was stopped.
    pub stop_reason: Option<StopReason>,
    /// Every stop that happened, with when, in time order.
    pub stops: Vec<(StopReason, Duration)>,
}

impl RunResult {
    /// The invocation outcome and exit code. Cancellation wins over the
    /// deadline, which wins over the first failing binary in selection order.
    /// Runner failures (cleanup, or event output when `runner_failed`) only
    /// turn an otherwise passing run into exit 1.
    pub fn verdict(&self, runner_failed: bool) -> (&'static str, i32) {
        match self.stop_reason {
            Some(reason @ StopReason::Signal(_)) => {
                ("cancelled", BinaryOutcome::from(reason).exit_code())
            }
            Some(reason @ StopReason::Deadline) => {
                ("timed-out", BinaryOutcome::from(reason).exit_code())
            }
            None => match self
                .outcomes
                .iter()
                .map(BinaryOutcome::exit_code)
                .find(|c| *c != 0)
            {
                Some(code) => ("failed", code),
                None if self.cleanup_failed || runner_failed => ("failed", 1),
                None => ("passed", 0),
            },
        }
    }
}

pub fn run(
    selection: &Selection,
    args: &[String],
    options: &Options,
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
        let outcome = supervisor.run(&mut command, !options.keep_leftovers, Some(&mut observer));
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
        ) || (!matches!(outcome, BinaryOutcome::Passed) && !options.no_fail_fast)
            || supervisor.cleanup_failed;
        outcomes.push(outcome);
    }
    let stop_reason = supervisor.stop_if_requested();
    RunResult {
        outcomes,
        stop_reason,
        // Only stops that the run observed; not a deadline passing just now.
        stops: if stop_reason.is_some() {
            supervisor.stops()
        } else {
            Vec::new()
        },
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
