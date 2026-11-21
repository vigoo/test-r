//! Supervise one command without interpreting its arguments or output.
use crate::run::{BinaryOutcome, StopReason};
use crate::supervise::Supervisor;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn run(
    command: &[String],
    started: Instant,
    deadline: Option<Duration>,
    kill_grace: Duration,
) -> anyhow::Result<i32> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("exec requires a command"))?;
    let mut supervisor = Supervisor::new(started, deadline, kill_grace)?;
    let mut child = Command::new(program);
    child
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let outcome = supervisor.run(&mut child, true, None);
    if matches!(outcome, BinaryOutcome::TimedOut) {
        eprintln!("Command deadline expired");
    }
    if let BinaryOutcome::LaunchFailed { error } = &outcome {
        eprintln!("Failed to launch command {program:?}: {error}");
    }
    // A deadline crossed only while cleaning up a completed command must not
    // replace its exit status. Cancellation during that cleanup still wins.
    if let Some(signal) = supervisor.stops().into_iter().find_map(|(reason, _)| {
        if let StopReason::Signal(signal) = reason {
            Some(signal)
        } else {
            None
        }
    }) {
        return Ok(128 + signal);
    }
    let code = match outcome {
        BinaryOutcome::LaunchFailed { .. } => 127,
        outcome => outcome.exit_code(),
    };
    Ok(if code == 0 && supervisor.cleanup_failed {
        1
    } else {
        code
    })
}
