//! Read-only validation of version 1 runner evidence. Report paths are opaque identities.
use crate::run::{BinaryOutcome, RunResult, StopReason};
use anyhow::{Context, bail, ensure};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub v: u8,
    pub invocation_id: Option<String>,
    pub complete: bool,
    pub outcome: Option<String>,
    pub reports: Vec<Report>,
    pub issues: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Report {
    pub path: String,
    pub format: Option<String>,
}

#[derive(Clone, Default)]
enum Phase {
    #[default]
    Ready,
    Running,
    Stopped {
        running: bool,
    },
    Finished(String),
}

impl Phase {
    fn running(&self) -> bool {
        matches!(self, Self::Running | Self::Stopped { running: true })
    }
}

#[derive(Clone, Default)]
struct State {
    id: Option<String>,
    seq: u64,
    elapsed: u64,
    admitted: Vec<String>,
    outcomes: Vec<BinaryOutcome>,
    phase: Phase,
    stop_reason: Option<StopReason>,
    stops: Vec<(StopReason, Duration)>,
}

fn string<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("invalid {key}"))
}

fn integer(value: &Value, key: &str) -> anyhow::Result<u64> {
    value[key]
        .as_u64()
        .with_context(|| format!("invalid {key}"))
}

fn exit_code(value: &Value) -> anyhow::Result<i32> {
    value["exit-code"]
        .as_i64()
        .and_then(|code| i32::try_from(code).ok())
        .context("invalid exit code")
}

fn timestamp(value: &str) -> bool {
    // The writer uses UTC with exactly millisecond precision, not arbitrary RFC3339.
    value.len() == 24
        && value.bytes().enumerate().all(|(i, c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'.',
            23 => c == b'Z',
            _ => c.is_ascii_digit(),
        })
        && value.parse::<jiff::Timestamp>().is_ok()
}

impl State {
    /// Validate against a private state copy. Neither reports nor coverage issues
    /// from a malformed record can leak into the committed prefix.
    fn apply(
        &mut self,
        value: &Value,
        expected_id: Option<&str>,
        report_format: Option<&str>,
    ) -> anyhow::Result<(Vec<Report>, Vec<String>)> {
        ensure!(value.is_object(), "event must be an object");
        ensure!(integer(value, "v")? == 1, "unsupported envelope version");
        ensure!(integer(value, "seq")? == self.seq, "noncontiguous sequence");
        let elapsed = integer(value, "elapsed-ms")?;
        ensure!(elapsed >= self.elapsed, "elapsed time decreased");
        ensure!(timestamp(string(value, "time")?), "invalid timestamp");
        let id = string(value, "invocation-id")?;
        ensure!(
            expected_id.is_none_or(|expected| expected == id),
            "unexpected invocation identity"
        );
        ensure!(
            self.id.as_deref().is_none_or(|prior| prior == id),
            "invocation identity changed"
        );
        ensure!(
            !matches!(self.phase, Phase::Finished(_)),
            "event after invocation finish"
        );
        let event = string(value, "event")?;
        ensure!(
            self.id.is_some() || event == "invocation-started",
            "missing invocation start"
        );
        let mut reports = Vec::new();
        let mut issues = Vec::new();
        match event {
            "invocation-started" => {
                ensure!(self.id.is_none(), "duplicate invocation start");
                {
                    let ids = value
                        .get("binary-ids")
                        .context("missing admitted binary ids")?;
                    let ids = ids.as_array().context("invalid admitted binary ids")?;
                    let mut admitted = BTreeSet::new();
                    for id in ids {
                        let id = id.as_str().context("invalid admitted binary id")?;
                        ensure!(
                            !id.is_empty() && admitted.insert(id.to_owned()),
                            "empty or duplicate admitted binary id"
                        );
                        self.admitted.push(id.to_owned());
                    }
                }
            }
            "binary-started" | "binary-launch-failed" | "binary-finished" => {
                let binary = string(value, "binary-id")?;
                ensure!(
                    !binary.is_empty() && integer(value, "attempt")? == 1,
                    "invalid binary attempt identity"
                );
                ensure!(
                    self.admitted.iter().any(|id| id == binary),
                    "binary not admitted"
                );
                ensure!(
                    self.admitted.get(self.outcomes.len()).map(String::as_str) == Some(binary),
                    "binary execution contradicts admitted order"
                );
                match event {
                    "binary-started" => {
                        ensure!(
                            matches!(self.phase, Phase::Ready),
                            "binary start during active execution or after stop"
                        );
                        self.phase = Phase::Running;
                    }
                    "binary-launch-failed" => {
                        ensure!(
                            matches!(self.phase, Phase::Ready),
                            "launch failure during active execution or after stop"
                        );
                        self.outcomes.push(BinaryOutcome::LaunchFailed {
                            error: string(value, "error")?.to_owned(),
                        });
                        if report_format.is_some() {
                            issues.push(format!(
                                "Binary {binary} failed to launch; report coverage is partial"
                            ));
                        }
                    }
                    _ => {
                        ensure!(self.phase.running(), "binary finish without start");
                        let outcome = string(value, "outcome")?;
                        let outcome = match outcome {
                            "passed" | "failed" => {
                                let code = exit_code(value)?;
                                ensure!(
                                    (outcome == "passed") == (code == 0),
                                    "binary outcome contradicts exit code"
                                );
                                ensure!(
                                    value.get("signal") == Some(&Value::Null),
                                    "unexpected binary signal"
                                );
                                if code == 0 {
                                    BinaryOutcome::Passed
                                } else {
                                    BinaryOutcome::Failed { code }
                                }
                            }
                            "crashed" => {
                                ensure!(
                                    value.get("exit-code") == Some(&Value::Null),
                                    "unexpected binary exit code"
                                );
                                let signal = i32::try_from(integer(value, "signal")?)?;
                                ensure!(
                                    signal > 0 && signal.checked_add(128).is_some(),
                                    "invalid binary signal"
                                );
                                BinaryOutcome::Crashed { signal }
                            }
                            "timed-out" | "cancelled" => {
                                ensure!(
                                    value.get("exit-code") == Some(&Value::Null)
                                        && value.get("signal") == Some(&Value::Null),
                                    "unexpected binary status"
                                );
                                if outcome == "timed-out" {
                                    ensure!(
                                        self.stops
                                            .iter()
                                            .any(|(reason, _)| *reason == StopReason::Deadline),
                                        "binary timeout without deadline"
                                    );
                                    BinaryOutcome::TimedOut
                                } else {
                                    let Some(reason @ StopReason::Signal(_)) = self.stop_reason
                                    else {
                                        bail!("binary cancellation without signal");
                                    };
                                    BinaryOutcome::from(reason)
                                }
                            }
                            _ => bail!("invalid binary outcome"),
                        };
                        let entries = value.get("reports").context("missing reports")?;
                        if entries.is_null() && report_format.is_some() {
                            issues.push(format!(
                                "Report identity unknown for binary {binary}: reports=null"
                            ));
                        } else if !entries.is_null() {
                            let entries = entries.as_array().context("invalid reports array")?;
                            if entries.is_empty() && report_format.is_some() {
                                issues.push(format!(
                                    "No report opened by binary {binary}: reports=[]"
                                ));
                            }
                            for entry in entries {
                                reports.push(Report {
                                    path: string(entry, "path")?.to_owned(),
                                    format: entry["format"].as_str().map(str::to_owned),
                                });
                            }
                            if let Some(format) = report_format
                                && !reports.is_empty()
                                && !reports
                                    .iter()
                                    .any(|report| report.format.as_deref() == Some(format))
                            {
                                issues.push(format!(
                                    "No {format} report identified for binary {binary}"
                                ));
                            }
                        }
                        self.outcomes.push(outcome);
                        self.phase = match self.phase {
                            Phase::Running => Phase::Ready,
                            _ => Phase::Stopped { running: false },
                        };
                    }
                }
            }
            "deadline-expired" | "cancel-requested" => {
                let reason = if event == "deadline-expired" {
                    ensure!(
                        !self
                            .stops
                            .iter()
                            .any(|(reason, _)| *reason == StopReason::Deadline),
                        "duplicate stop event"
                    );
                    ensure!(
                        value.get("signal") == Some(&Value::Null),
                        "unexpected deadline signal"
                    );
                    StopReason::Deadline
                } else {
                    ensure!(
                        !matches!(self.stop_reason, Some(StopReason::Signal(_))),
                        "duplicate stop event"
                    );
                    let signal = i32::try_from(integer(value, "signal")?)?;
                    ensure!(
                        signal > 0 && signal.checked_add(128).is_some(),
                        "invalid cancellation signal"
                    );
                    StopReason::Signal(signal)
                };
                ensure!(
                    integer(value, "at-elapsed-ms")? <= elapsed,
                    "stop time exceeds elapsed time"
                );
                if let Some(binary) = value.get("binary-id").filter(|v| !v.is_null()) {
                    let binary = binary.as_str().context("invalid stop binary id")?;
                    ensure!(
                        self.admitted
                            .iter()
                            .take(self.outcomes.len() + usize::from(self.phase.running()))
                            .any(|id| id == binary),
                        "stop for unknown binary"
                    );
                }
                if self.stop_reason.is_none() || matches!(reason, StopReason::Signal(_)) {
                    self.stop_reason = Some(reason);
                }
                self.stops.push((
                    reason,
                    Duration::from_millis(integer(value, "at-elapsed-ms")?),
                ));
                self.phase = Phase::Stopped {
                    running: self.phase.running(),
                };
            }
            "invocation-finished" => {
                ensure!(
                    !self.phase.running(),
                    "invocation finished with active binaries"
                );
                let outcome = string(value, "outcome")?;
                let code = exit_code(value)?;
                let not_run = self.admitted.len() - self.outcomes.len();
                let mut result = RunResult {
                    outcomes: self.outcomes.clone(),
                    cleanup_failed: false,
                    stop_reason: self.stop_reason,
                    stops: self.stops.clone(),
                };
                result
                    .outcomes
                    .extend((0..not_run).map(|_| BinaryOutcome::NotRun));
                // Version 1 does not identify cleanup/event-output failures.
                // Either native verdict is valid; runner failure cannot replace
                // a binary failure code or an observed stop reason.
                ensure!(
                    [result.verdict(false), result.verdict(true)].contains(&(outcome, code)),
                    "invocation outcome contradicts exit code or native verdict"
                );
                for name in [
                    "passed",
                    "failed",
                    "crashed",
                    "launch-failed",
                    "timed-out",
                    "cancelled",
                    "not-run",
                ] {
                    ensure!(
                        integer(value, name)?
                            == result
                                .outcomes
                                .iter()
                                .filter(|outcome| outcome.name() == name)
                                .count() as u64,
                        "incorrect {name} count"
                    );
                }
                if outcome == "passed" {
                    ensure!(
                        not_run == 0,
                        "passing invocation contradicts binary coverage"
                    );
                }
                if not_run > 0 && report_format.is_some() {
                    issues.push(format!(
                        "{not_run} selected binaries did not run; report coverage is partial"
                    ));
                }
                self.phase = Phase::Finished(outcome.to_owned());
            }
            _ => bail!("unknown event kind"),
        }
        self.id = Some(id.to_owned());
        self.seq = self.seq.checked_add(1).context("sequence overflow")?;
        self.elapsed = elapsed;
        Ok((reports, issues))
    }
}

/// Inspect only newline-terminated records. Stream problems are evidence issues,
/// not CLI failures. No report path is opened or otherwise interpreted.
pub fn inspect(
    path: &Path,
    expected_id: Option<&str>,
    report_format: Option<&str>,
) -> anyhow::Result<Inspection> {
    let mut result = Inspection {
        v: 1,
        invocation_id: None,
        complete: false,
        outcome: None,
        reports: Vec::new(),
        issues: Vec::new(),
    };
    let mut state = State::default();
    match std::fs::File::open(path) {
        Err(error) => result
            .issues
            .push(format!("Unreadable runner stream: {error}")),
        Ok(file) => {
            let mut reader = BufReader::new(file);
            let mut line = Vec::new();
            let mut number = 0;
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Err(error) => {
                        result
                            .issues
                            .push(format!("Unreadable runner stream: {error}"));
                        break;
                    }
                    Ok(_) => {}
                }
                number += 1;
                if !line.ends_with(b"\n") {
                    result.issues.push("Truncated last line ignored".into());
                    break;
                }
                let mut next = state.clone();
                let parsed = serde_json::from_slice::<Value>(&line)
                    .map_err(anyhow::Error::from)
                    .and_then(|value| next.apply(&value, expected_id, report_format));
                match parsed {
                    Ok((reports, issues)) => {
                        state = next;
                        result.reports.extend(reports);
                        result.issues.extend(issues);
                    }
                    Err(error) => {
                        result.issues.push(format!("Malformed runner event at line {number}: {error}; subsequent records ignored"));
                        break;
                    }
                }
            }
        }
    }
    if let Phase::Finished(outcome) = state.phase {
        result.outcome = Some(outcome);
    } else {
        result
            .issues
            .push("Missing invocation-finished record; runner evidence is incomplete".into());
    }
    result.invocation_id = state.id;
    result.complete = result.outcome.is_some() && result.issues.is_empty();
    result.reports.sort();
    result.reports.dedup();
    Ok(result)
}
