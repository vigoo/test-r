//! Blocking preparation runs in a contained worker, never in the signal owner.
use crate::run::BinaryOutcome;
use crate::supervise::Supervisor;
use anyhow::Context;
use camino::Utf8Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `args` starts with the preparation subcommand, without the executable name.
/// The worker receives the same arguments plus `--preparation-worker` and, for
/// snapshots, `--staging-directory`. It must not create another supervisor.
pub fn run(
    args: &[String],
    started: Instant,
    deadline: Option<Duration>,
    kill_grace: Duration,
    snapshot_out: Option<&Utf8Path>,
) -> anyhow::Result<i32> {
    let mut supervisor = Supervisor::new(started, deadline, kill_grace)?;
    // In particular, a zero budget must not create staging or launch a worker.
    if let Some(reason) = supervisor.stop_if_requested() {
        return Ok(BinaryOutcome::from(reason).exit_code());
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(&args[0])
        .arg("--preparation-worker")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // The outer directory remains owned by this process even if the worker is
    // killed between creating a temporary file and publishing the snapshot.
    let staging = snapshot_out
        .map(|out| {
            let out = std::path::absolute(out)?;
            let parent = out
                .parent()
                .context("Snapshot output needs a parent directory")?;
            tempfile::Builder::new()
                .prefix(".cargo-test-r-preparation-")
                .tempdir_in(parent)
                .context("Cannot create snapshot staging root")
        })
        .transpose()?;
    if let Some(staging) = &staging {
        command.arg("--staging-directory").arg(staging.path());
    }
    // Internal options precede the caller's possible end-of-options marker.
    command.args(&args[1..]);
    // Cleanup is mandatory even on success: loaders may leave descendants.
    let outcome = supervisor.run(&mut command, true, None);
    let outcome = supervisor
        .stop_if_requested()
        .map_or(outcome, BinaryOutcome::from);
    if supervisor.cleanup_failed {
        // Do not remove a directory which undrained processes may still use.
        if let Some(staging) = staging {
            eprintln!(
                "Retained snapshot staging root after failed process cleanup: {}",
                staging.keep().display()
            );
        }
        return Ok(if outcome.exit_code() == 0 {
            1
        } else {
            outcome.exit_code()
        });
    }
    if let Some(staging) = staging {
        // Successful publication renamed this directory. Failed preparation
        // leaves it here until every descendant has been drained.
        if let Err(error) = staging.close()
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error).context("Cannot remove snapshot staging root");
        }
    }
    if let BinaryOutcome::LaunchFailed { error } = &outcome {
        eprintln!("Cannot launch preparation worker: {error}");
    }
    Ok(outcome.exit_code())
}
