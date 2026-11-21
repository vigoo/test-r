mod common;

use common::{cargo_test_r, exe, install};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// A portable, real executable with distinguishable outcomes. Only `index`
// writes the report index: the others also exercise older binaries.
fn fixture() -> PathBuf {
    common::rustc_fixture(
        r##"
fn main() {
    let mode = std::env::current_exe().unwrap().file_stem().unwrap().to_str().unwrap().to_owned();
    let identity = ["TEST_R_INVOCATION_ID", "TEST_R_BINARY_ID", "TEST_R_ATTEMPT", "TEST_R_REPORT_INDEX"]
        .map(|key| std::env::var(key).unwrap_or_default()).join("\n");
    std::fs::write(format!("ran-{mode}"), identity).unwrap();
    if mode == "index" {
        let lines = [r#"{"path":"/a","format":"ctrf"}"#, "not json", r#""/legacy-string""#, r#"{"path":"/b","extra":1}"#];
        std::fs::write(std::env::var("TEST_R_REPORT_INDEX").unwrap(), lines.join("\n") + "\n").unwrap();
    }
    if mode == "fail" { std::process::exit(7); }
    if mode == "crash" { std::process::abort(); }
    if mode == "hung" { std::thread::sleep(std::time::Duration::from_secs(30)); }
    #[cfg(unix)]
    #[cfg(unix)]
    if mode == "daemon" {
        // Leaves a stubborn child behind and exits normally once it is ready.
        let dir = std::env::current_exe().unwrap().parent().unwrap().to_owned();
        std::process::Command::new(dir.join("stubborn")).spawn().unwrap();
        while !std::path::Path::new("stubborn-ready").exists() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[cfg(unix)]
    if mode == "stubborn" {
        unsafe extern "C" { fn signal(signum: i32, handler: usize) -> usize; }
        // SIG_IGN for SIGINT and SIGTERM: only KILL ends it.
        unsafe { signal(2, 1); signal(15, 1); }
        std::fs::write("stubborn-ready", "").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}
"##,
    )
}

struct Suite {
    dir: tempfile::TempDir,
}

impl Suite {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        install(
            &fixture(),
            dir.path(),
            &["ok", "fail", "hung", "crash", "index", "stubborn", "daemon"],
        );
        Self { dir }
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    fn selection(&self, executables: &[PathBuf]) {
        common::write_json(
            &self.path().join("selection.json"),
            &common::selection(executables, self.path()),
        );
    }
    fn command(&self, names: &[&str]) -> Command {
        self.selection(
            &names
                .iter()
                .map(|name| self.path().join(exe(name)))
                .collect::<Vec<_>>(),
        );
        self.run("events.jsonl")
    }
    fn run(&self, log: &str) -> Command {
        let mut command = cargo_test_r();
        command.current_dir(self.path()).args([
            "run",
            "--selection",
            "selection.json",
            "--events",
            log,
        ]);
        command
    }
    fn records(&self, log: &str) -> Vec<Value> {
        let raw = std::fs::read_to_string(self.path().join(log)).unwrap();
        assert!(raw.ends_with('\n'));
        let records: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (i, record) in records.iter().enumerate() {
            assert_eq!(record["v"], 1);
            assert_eq!(record["seq"], i);
            record["time"]
                .as_str()
                .unwrap()
                .parse::<jiff::Timestamp>()
                .unwrap();
            if i > 0 {
                assert!(record["elapsed-ms"].as_u64() >= records[i - 1]["elapsed-ms"].as_u64());
            }
        }
        records
    }
}

#[test]
fn filtered_selection_events_identify_the_parent_manifest_and_admitted_subset() {
    let suite = Suite::new();
    let mut command = suite.command(&["fail", "ok", "index"]);
    let manifest = suite.path().join("selection.json");
    let original = std::fs::read(&manifest).unwrap();
    let output = command
        .args(["--binary-id", "pkg::1", "--expect-count", "1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let records = suite.records("events.jsonl");
    assert_eq!(
        records[0]["selection-sha256"],
        common::sha256_hex(&original)
    );
    assert_eq!(records[0]["binary-ids"], json!(["pkg::1"]));
    assert_eq!(records[1]["binary-id"], "pkg::1");
    assert_eq!(records.last().unwrap()["passed"], 1);
    assert_eq!(records.last().unwrap()["not-run"], 0);
    assert_eq!(std::fs::read(manifest).unwrap(), original);
    assert!(suite.path().join("ran-ok").exists());
    assert!(!suite.path().join("ran-fail").exists());
    assert!(!suite.path().join("ran-index").exists());
}

#[test]
fn outcomes_order_identity_and_not_run_counts() {
    for (names, flags, code, events) in [
        (
            vec!["ok"],
            vec![],
            0,
            vec![
                "invocation-started",
                "binary-started",
                "binary-finished",
                "invocation-finished",
            ],
        ),
        (
            vec!["fail", "ok"],
            vec![],
            7,
            vec![
                "invocation-started",
                "binary-started",
                "binary-finished",
                "invocation-finished",
            ],
        ),
        (
            vec!["fail", "ok"],
            vec!["--no-fail-fast"],
            7,
            vec![
                "invocation-started",
                "binary-started",
                "binary-finished",
                "binary-started",
                "binary-finished",
                "invocation-finished",
            ],
        ),
    ] {
        let suite = Suite::new();
        let output = suite
            .command(&names)
            .args(flags)
            .args(["--invocation-id", "opaque / shard 3", "--", "harness-arg"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        let records = suite.records("events.jsonl");
        assert_eq!(
            records
                .iter()
                .map(|r| r["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            events
        );
        assert!(
            records
                .iter()
                .all(|r| r["invocation-id"] == "opaque / shard 3")
        );
        let hash = common::sha256_hex(&std::fs::read(suite.path().join("selection.json")).unwrap());
        assert_eq!(records[0]["selection-sha256"], hash);
        assert_eq!(records[1]["args"], json!(["harness-arg"]));
        assert_eq!(records[1]["cwd"], json!(suite.path()));
        assert_eq!(records[1]["package-id"], "pkg@0.1.0");
        assert_eq!(records[1]["attempt"], 1);
        let identity =
            std::fs::read_to_string(suite.path().join(format!("ran-{}", names[0]))).unwrap();
        let identity: Vec<_> = identity.lines().collect();
        assert_eq!(&identity[..3], ["opaque / shard 3", "pkg::0", "1"]);
        assert!(
            !Path::new(identity[3]).parent().unwrap().exists(),
            "private indexes must be cleaned up"
        );
        assert!(records[1]["pid"].as_u64().unwrap() > 0);
        assert!(records[2]["reports"].is_null());
        assert_eq!(
            records[2]["exit-code"],
            if names[0] == "fail" { 7 } else { 0 }
        );
        assert!(records[2]["duration-ms"].is_u64());
        let last = records.last().unwrap();
        assert_eq!(last["exit-code"], code);
        assert_eq!(
            last["not-run"],
            usize::from(names.len() == 2 && records.len() == 4)
        );
        assert_eq!(last["failed"], usize::from(code == 7));
    }
}

#[test]
fn existing_log_and_cargo_fallback_are_rejected_without_execution() {
    let suite = Suite::new();
    std::fs::write(suite.path().join("events.jsonl"), "retain me").unwrap();
    let output = suite.command(&["ok"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!suite.path().join("ran-ok").exists());
    assert_eq!(
        std::fs::read_to_string(suite.path().join("events.jsonl")).unwrap(),
        "retain me"
    );
    std::fs::write(suite.path().join("Cargo.lock"), "").unwrap();
    let output = cargo_test_r()
        .current_dir(suite.path())
        .args(["run", "--target-dir", "absent", "--events", "new.jsonl"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--events is supported only for prebuilt")
    );
    assert!(!suite.path().join("new.jsonl").exists());
}

#[cfg(unix)]
#[test]
fn launch_failure_and_crash_are_distinct() {
    use std::os::unix::fs::PermissionsExt;
    let suite = Suite::new();
    let invalid = suite.path().join("invalid");
    std::fs::write(&invalid, "#!/cargo-test-r-missing-interpreter\n").unwrap();
    std::fs::set_permissions(&invalid, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = suite
        .command(&["invalid", "ok"])
        .arg("--no-fail-fast")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let records = suite.records("events.jsonl");
    assert_eq!(records[1]["event"], "binary-launch-failed");
    assert!(!records[1]["error"].as_str().unwrap().is_empty());
    assert_eq!(records.last().unwrap()["launch-failed"], 1);
    assert_eq!(records.last().unwrap()["passed"], 1);
    suite.command(&["crash"]);
    let output = suite.run("crash.jsonl").output().unwrap();
    assert_eq!(output.status.code(), Some(128 + libc::SIGABRT));
    let records = suite.records("crash.jsonl");
    assert_eq!(records[2]["signal"], libc::SIGABRT);
    assert!(records[2]["exit-code"].is_null());
}

#[cfg(unix)]
fn wait_started(suite: &Suite, log: &str) -> u32 {
    common::wait_until("binary-started record", || {
        let raw = std::fs::read_to_string(suite.path().join(log)).ok()?;
        raw.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|record| record["event"] == "binary-started")
            .map(|record| record["pid"].as_u64().unwrap() as u32)
    })
}

/// A signal during cleanup is logged after the deadline that started it, or
/// that expired during it, and decides the outcome. Each stop carries the
/// time it happened, which can be earlier than when it was written.
#[cfg(unix)]
#[test]
fn signal_during_cleanup_is_recorded_with_the_deadline() {
    // Stopped by the deadline: the binary ignores TERM for the whole grace.
    // Cleaning up after a normal exit: the binary's child ignores TERM, and
    // the deadline expires during that cleanup.
    for (binary, deadline_ms, finished_before_stop) in
        [("stubborn", 2000, false), ("daemon", 1000, true)]
    {
        let suite = Suite::new();
        let started = std::time::Instant::now();
        let child = suite
            .command(&[binary])
            .args([
                "--deadline",
                &format!("{deadline_ms}ms"),
                "--kill-grace",
                "3s",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        common::wait_until("stubborn process ignoring signals", || {
            suite.path().join("stubborn-ready").exists().then_some(())
        });
        if finished_before_stop {
            // No record is written while cleanup runs; wait past the deadline.
            std::thread::sleep(
                std::time::Duration::from_millis(deadline_ms + 600)
                    .saturating_sub(started.elapsed()),
            );
        } else {
            common::wait_until("deadline-expired record", || {
                let raw = std::fs::read_to_string(suite.path().join("events.jsonl")).ok()?;
                raw.contains("\"deadline-expired\"").then_some(())
            });
        }
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
        let output = common::finish(child);
        assert_eq!(output.status.code(), Some(130), "{binary}: {output:?}");
        let records: Vec<_> = suite
            .records("events.jsonl")
            .into_iter()
            .filter(|r| r["event"] != "binary-started")
            .collect();
        let events: Vec<_> = records
            .iter()
            .map(|r| r["event"].as_str().unwrap())
            .collect();
        let mut expected = vec![
            "invocation-started",
            "deadline-expired",
            "cancel-requested",
            "binary-finished",
            "invocation-finished",
        ];
        if finished_before_stop {
            expected.retain(|e| *e != "binary-finished");
            expected.insert(1, "binary-finished");
        }
        assert_eq!(events, expected, "{binary}");
        let deadline = &records[events
            .iter()
            .position(|e| *e == "deadline-expired")
            .unwrap()];
        let cancel = &records[events
            .iter()
            .position(|e| *e == "cancel-requested")
            .unwrap()];
        assert_eq!(deadline["at-elapsed-ms"], deadline_ms, "{binary}");
        assert_eq!(cancel["signal"], libc::SIGINT, "{binary}");
        let cancel_at = cancel["at-elapsed-ms"].as_u64().unwrap();
        assert!(cancel_at > deadline_ms, "{binary}: {cancel}");
        assert!(
            cancel_at <= cancel["elapsed-ms"].as_u64().unwrap(),
            "{binary}: {cancel}"
        );
        assert_eq!(records.last().unwrap()["outcome"], "cancelled", "{binary}");
    }
}

#[cfg(unix)]
#[test]
fn cancellation_and_abrupt_death_do_not_manufacture_completion() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGKILL] {
        let suite = Suite::new();
        let mut child = suite
            .command(&["hung", "ok"])
            .args(["--kill-grace", "0ms"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let binary_pid = wait_started(&suite, "events.jsonl");
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        let status = child.wait().unwrap();
        let records = suite.records("events.jsonl");
        if signal == libc::SIGKILL {
            assert!(records.iter().all(|r| r["event"] != "invocation-finished"));
            unsafe {
                libc::kill(-(binary_pid as i32), libc::SIGKILL);
            }
        } else {
            assert_eq!(status.code(), Some(128 + signal));
            assert!(
                records
                    .iter()
                    .any(|r| r["event"] == "cancel-requested" && r["signal"] == signal)
            );
            assert_eq!(records.last().unwrap()["outcome"], "cancelled");
            assert_eq!(records.last().unwrap()["exit-code"], 128 + signal);
            assert_eq!(records.last().unwrap()["not-run"], 1);
        }
    }
}

#[test]
fn report_index_lines_without_a_string_path_are_skipped() {
    let suite = Suite::new();
    let output = suite.command(&["index"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let records = suite.records("events.jsonl");
    assert_eq!(records[2]["event"], "binary-finished");
    // Objects pass through unchanged, including fields this version ignores.
    assert_eq!(
        records[2]["reports"],
        json!([{"path": "/a", "format": "ctrf"}, {"path": "/b", "extra": 1}])
    );
}

#[test]
fn deadline_records_stop_before_finish() {
    let suite = Suite::new();
    let output = suite
        .command(&["hung", "ok"])
        .args(["--deadline", "500ms", "--kill-grace", "0ms"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(124));
    let records = suite.records("events.jsonl");
    assert_eq!(records[2]["event"], "deadline-expired");
    assert_eq!(records[3]["outcome"], "timed-out");
    assert_eq!(records[4]["not-run"], 1);
    assert_eq!(records[4]["exit-code"], 124);
}

#[cfg(unix)]
#[test]
fn later_log_write_failure_does_not_stop_tests_or_hide_failure() {
    use std::os::unix::process::CommandExt;
    for (binary_code, code) in [(0, 1), (7, 7)] {
        let suite = Suite::new();
        suite.selection(&[PathBuf::from("/bin/sh")]);
        let mut command = suite.run("events.jsonl");
        command.args(["--", "-c", &format!("printf executed; exit {binary_code}")]);
        // Real kernel write failure, without a production test hook. The file
        // can be created but no bytes can be written; SIGXFSZ must be ignored.
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
                let limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        assert_eq!(output.stdout, b"executed");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Event log write failed"));
    }
}

#[test]
fn without_events_needs_no_temp_directory_or_report_index() {
    let suite = Suite::new();
    suite.command(&["ok"]);
    let output = cargo_test_r()
        .current_dir(suite.path())
        .args([
            "run",
            "--selection",
            "selection.json",
            "--invocation-id",
            "plain",
        ])
        .env("TMPDIR", suite.path().join("missing"))
        .env("TMP", suite.path().join("missing"))
        .env("TEMP", suite.path().join("missing"))
        .env("TEST_R_REPORT_INDEX", suite.path().join("inherited-index"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let identity = std::fs::read_to_string(suite.path().join("ran-ok")).unwrap();
    assert_eq!(identity, "plain\npkg::0\n1\n");
    assert!(!suite.path().join("inherited-index").exists());
}

#[test]
fn real_retry_reports_and_concurrent_invocations_are_attributed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for sync in [true, false] {
        let mut build = Command::new("cargo");
        build.current_dir(root).args([
            "test",
            "-p",
            "test-r",
            "--test",
            "retry_failed_fixture",
            "--no-run",
            "--message-format=json",
        ]);
        if sync {
            build.arg("--no-default-features");
        }
        let output = build.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let executable: PathBuf = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|v| {
                if v["target"]["name"] == "retry_failed_fixture" {
                    v["executable"].as_str().map(PathBuf::from)
                } else {
                    None
                }
            })
            .unwrap();
        let suite = Suite::new();
        suite.selection(std::slice::from_ref(&executable));
        let mut children = Vec::new();
        for id in ["a", "b"] {
            let child = suite
                .run(&format!("{id}.jsonl"))
                .env("TEST_R_RETRY_FAILED_DIR", suite.path().join(id))
                .args([
                    "--",
                    "panics_once",
                    "--include-ignored",
                    "--retry-failed",
                    "1",
                    "--format",
                    "ctrf",
                    "--logfile",
                ])
                .arg(format!("{id}/report.json"))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            children.push((id, child));
        }
        let mut ids = Vec::new();
        let mut paths = Vec::new();
        for (id, child) in children {
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let records = suite.records(&format!("{id}.jsonl"));
            let invocation = records[0]["invocation-id"].as_str().unwrap();
            uuid::Uuid::parse_str(invocation).unwrap();
            assert!(records.iter().all(|r| r["invocation-id"] == invocation));
            ids.push(invocation.to_string());
            assert_eq!(records[2]["outcome"], "passed");
            let reports = records[2]["reports"].as_array().unwrap();
            assert_eq!(reports.len(), 1);
            let path = PathBuf::from(reports[0]["path"].as_str().unwrap());
            assert_eq!(reports[0], json!({"path": path, "format": "ctrf"}));
            assert!(path.is_absolute());
            assert_eq!(
                path.parent().unwrap().canonicalize().unwrap(),
                suite.path().join(id).canonicalize().unwrap()
            );
            let stem = path.file_stem().unwrap().to_str().unwrap();
            uuid::Uuid::parse_str(stem.strip_prefix("report-").unwrap()).unwrap();
            let report: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(report["results"]["tests"][0]["status"], "passed");
            assert_eq!(
                std::fs::read_to_string(suite.path().join(id).join("panics_once")).unwrap(),
                "xx"
            );
            paths.push(path);
        }
        assert_ne!(ids[0], ids[1]);
        assert_ne!(paths[0], paths[1]);
        // Current test-r with no logfile positively acknowledges no reports.
        let output = suite
            .run("none.jsonl")
            .args(["--", "passes"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(suite.records("none.jsonl")[2]["reports"], json!([]));
        // Auxiliary index errors must not break real tests or report creation.
        let output = Command::new(&executable)
            .current_dir(suite.path())
            .env("TEST_R_RETRY_FAILED_DIR", suite.path().join("warning"))
            .env("TEST_R_REPORT_INDEX", suite.path())
            .args([
                "passes",
                "--include-ignored",
                "--format",
                "ctrf",
                "--logfile",
                "warning.json",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("Warning: cannot append TEST_R_REPORT_INDEX")
        );
        for nocapture in [false, true] {
            let name = if nocapture {
                "outer-nocapture"
            } else {
                "outer-capture"
            };
            let mut command = suite.run(&format!("{name}.jsonl"));
            command
                .env("TEST_R_RETRY_FAILED_DIR", suite.path().join(name))
                .args([
                    "--",
                    "nested_report",
                    "--include-ignored",
                    "--format",
                    "ctrf",
                    "--logfile",
                ])
                .arg(format!("{name}.json"));
            if nocapture {
                command.arg("--nocapture");
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let records = suite.records(&format!("{name}.jsonl"));
            let reports = records[2]["reports"].as_array().unwrap();
            assert_eq!(
                reports.len(),
                1,
                "nested reports must not be attributed to the outer binary: {reports:?}"
            );
            assert_eq!(reports[0]["format"], "ctrf", "{reports:?}");
            let outer = Path::new(reports[0]["path"].as_str().unwrap());
            assert!(
                outer
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&format!("{name}-"))
            );
            let nested: Vec<_> = std::fs::read_dir(suite.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("nested-")
                        && path
                            .extension()
                            .is_some_and(|extension| extension == "json")
                })
                .collect();
            assert_eq!(nested.len(), if nocapture { 2 } else { 1 });
            for path in nested {
                let report: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                assert_eq!(report["results"]["tests"][0]["status"], "passed");
            }
        }
    }
}
