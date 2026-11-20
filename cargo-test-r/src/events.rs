//! Version 1 invocation evidence, independent of harness stdout/stderr.
use crate::run::{BinaryOutcome, Options, RunResult, StopReason};
use crate::selection::SelectedBinary;
use anyhow::Context;
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

pub struct Events {
    file: Option<File>,
    pub failed: bool,
    seq: u64,
    started: Instant,
    pub invocation_id: String,
    indexes: Option<tempfile::TempDir>,
    stop_recorded: bool,
}

impl Events {
    pub fn new(path: Option<&Path>, id: Option<String>, started: Instant) -> anyhow::Result<Self> {
        let indexes = path
            .map(|_| tempfile::tempdir().context("creating private report index directory"))
            .transpose()?;
        let file = path
            .map(|path| {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .with_context(|| format!("creating event log {}", path.display()))
            })
            .transpose()?;
        Ok(Self {
            file,
            failed: false,
            seq: 0,
            started,
            invocation_id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            indexes,
            stop_recorded: false,
        })
    }

    pub fn record(&mut self, event: &str, mut fields: Value) {
        if self.failed || self.file.is_none() {
            return;
        }
        let object = fields.as_object_mut().expect("event fields are an object");
        object.insert("v".into(), json!(1));
        object.insert("seq".into(), json!(self.seq));
        object.insert(
            "time".into(),
            json!(format!("{:.3}", jiff::Timestamp::now())),
        );
        object.insert(
            "elapsed-ms".into(),
            json!(self.started.elapsed().as_millis() as u64),
        );
        object.insert("invocation-id".into(), json!(self.invocation_id));
        object.insert("event".into(), json!(event));
        let mut line = serde_json::to_vec(&fields).expect("JSON event serialization");
        line.push(b'\n');
        let file = self.file.as_mut().unwrap();
        let result = file
            .write_all(&line)
            .and_then(|()| file.flush())
            .and_then(|()| {
                if event == "invocation-finished" {
                    file.sync_all()
                } else {
                    Ok(())
                }
            });
        self.seq += 1;
        if let Err(error) = result {
            eprintln!("Event log write failed: {error}");
            self.failed = true;
        }
    }

    /// `selection_sha256` identifies the binaries: the saved selection's bytes,
    /// or the resolved selection's compact JSON for metadata and archive input.
    pub fn started(&mut self, selection_sha256: &str, options: &Options) {
        self.record(
            "invocation-started",
            json!({
                "pid": std::process::id(), "selection-sha256": selection_sha256,
                "no-fail-fast": options.no_fail_fast,
                "deadline-ms": options.deadline.map(|d| d.as_millis() as u64),
                "kill-grace-ms": options.kill_grace.as_millis() as u64,
                "keep-leftovers": options.keep_leftovers
            }),
        );
    }

    pub fn stop(&mut self, reason: StopReason, binary: Option<&str>) {
        if self.stop_recorded {
            return;
        }
        let (event, signal) = match reason {
            StopReason::Deadline => ("deadline-expired", None),
            StopReason::Signal(signal) => ("cancel-requested", Some(signal)),
        };
        self.stop_recorded = true;
        self.record(event, json!({"binary-id": binary, "signal": signal}));
    }

    pub fn binary<'a>(
        &'a mut self,
        binary: &'a SelectedBinary,
        args: &'a [String],
        index: usize,
    ) -> BinaryEvents<'a> {
        let report_index = self
            .indexes
            .as_ref()
            .map(|dir| dir.path().join(index.to_string()));
        BinaryEvents {
            events: self,
            binary,
            args,
            report_index,
            started: Instant::now(),
            spawned: false,
        }
    }

    pub fn finish(&mut self, result: &RunResult) -> i32 {
        if let Some(reason) = result.stop_reason {
            self.stop(reason, None);
        }
        let (outcome, code) = result.verdict(self.failed);
        let mut fields = json!({"outcome": outcome, "exit-code": code});
        for name in [
            "passed",
            "failed",
            "crashed",
            "launch-failed",
            "timed-out",
            "cancelled",
            "not-run",
        ] {
            fields[name] = json!(result.outcomes.iter().filter(|o| o.name() == name).count());
        }
        self.record("invocation-finished", fields);
        // A failed final write still must not report success.
        result.verdict(self.failed).1
    }
}

pub struct BinaryEvents<'a> {
    events: &'a mut Events,
    binary: &'a SelectedBinary,
    args: &'a [String],
    report_index: Option<std::path::PathBuf>,
    started: Instant,
    spawned: bool,
}

impl BinaryEvents<'_> {
    pub fn configure(&self, command: &mut std::process::Command) {
        command
            .env_remove("TEST_R_REPORT_INDEX")
            .env("TEST_R_INVOCATION_ID", &self.events.invocation_id)
            .env("TEST_R_BINARY_ID", &self.binary.binary_id)
            .env("TEST_R_ATTEMPT", "1");
        if let Some(index) = &self.report_index {
            command.env("TEST_R_REPORT_INDEX", index);
        }
    }
    pub fn started(&mut self, pid: u32) {
        self.spawned = true;
        self.events.record(
            "binary-started",
            json!({"binary-id": self.binary.binary_id,
            "package-id": self.binary.package_id, "attempt": 1, "pid": pid,
            "cwd": self.binary.working_directory, "args": self.args}),
        );
    }
    pub fn stop(&mut self, reason: StopReason) {
        self.events.stop(reason, Some(&self.binary.binary_id));
    }
    pub fn finished(&mut self, outcome: &BinaryOutcome) {
        if !self.spawned && !matches!(outcome, BinaryOutcome::LaunchFailed { .. }) {
            return;
        }
        // Each index line is an object with at least a string `path`. Lines
        // this version does not understand are skipped, so newer harnesses
        // can extend the format without hiding the reports older runners see.
        let reports: Option<Vec<Value>> = self
            .report_index
            .as_ref()
            .and_then(|index| std::fs::read_to_string(index).ok())
            .map(|text| {
                text.lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter(|report| report["path"].is_string())
                    .collect()
            });
        let (code, signal) = match outcome {
            BinaryOutcome::Passed => (Some(0), None),
            BinaryOutcome::Failed { code } => (Some(*code), None),
            BinaryOutcome::Crashed { signal } => (None, Some(*signal)),
            _ => (None, None),
        };
        if let BinaryOutcome::LaunchFailed { error } = outcome {
            self.events.record(
                "binary-launch-failed",
                json!({"binary-id": self.binary.binary_id,
                "package-id": self.binary.package_id, "attempt": 1, "error": error}),
            );
        } else {
            self.events.record(
                "binary-finished",
                json!({"binary-id": self.binary.binary_id,
                "attempt": 1, "outcome": outcome.name(), "exit-code": code, "signal": signal,
                "duration-ms": self.started.elapsed().as_millis() as u64,
                "leftovers-killed": null, "reports": reports}),
            );
        }
    }
}
