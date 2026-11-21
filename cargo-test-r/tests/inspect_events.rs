mod common;

use common::{cargo_test_r, stdout_json};
use serde_json::{Value, json};
use std::path::Path;

fn records() -> Vec<Value> {
    sequence(vec![
        json!({"event":"invocation-started", "binary-ids":["a"]}),
        json!({"event":"binary-started", "binary-id":"a", "attempt":1}),
        json!({"event":"binary-finished", "binary-id":"a", "attempt":1,
            "outcome":"passed", "exit-code":0, "signal":null,
            "reports":[{"path":"/not-opened/report-retry-2.json", "format":"ctrf"}]}),
        json!({"event":"invocation-finished", "outcome":"passed", "exit-code":0, "not-run":0, "passed":1,
            "failed":0, "crashed":0, "launch-failed":0, "timed-out":0, "cancelled":0}),
    ])
}

fn sequence(events: Vec<Value>) -> Vec<Value> {
    events
        .into_iter()
        .enumerate()
        .map(|(i, mut v)| {
            v["v"] = json!(1);
            v["seq"] = json!(i);
            v["elapsed-ms"] = json!(i);
            v["time"] = json!("2026-11-21T01:02:03.004Z");
            v["invocation-id"] = json!("opaque / invocation");
            v
        })
        .collect()
}

fn write(path: &Path, records: &[Value]) {
    let text: String = records.iter().map(|v| format!("{v}\n")).collect();
    std::fs::write(path, text).unwrap();
}

fn inspect(path: &Path, expected: Option<&str>) -> Value {
    let mut cmd = cargo_test_r();
    cmd.arg("inspect-events").arg("--events").arg(path);
    if let Some(id) = expected {
        cmd.args(["--invocation-id", id]);
    }
    stdout_json(cmd.output().unwrap())
}

fn issue(value: &Value, needle: &str) {
    assert!(
        value["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains(needle)),
        "{value}"
    );
    assert_eq!(value["complete"], false, "{value}");
}

#[test]
fn valid_stream_has_exact_public_shape_and_opaque_report_identities() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("arbitrary-name");
    let mut events = records();
    events[2]["reports"] = json!([
        {"path":"../not-confined/retry-2.xml", "format":"junit"},
        {"path":"/not-opened/report-retry-2.json", "format":"ctrf"},
        {"path":"/not-opened/report-retry-3.json", "format":"ctrf"}
    ]);
    write(&path, &events);
    assert_eq!(
        inspect(&path, Some("opaque / invocation")),
        json!({
            "v":1, "invocation_id":"opaque / invocation", "complete":true,
            "outcome":"passed", "issues":[], "reports":[
                {"path":"../not-confined/retry-2.xml", "format":"junit"},
                {"path":"/not-opened/report-retry-2.json", "format":"ctrf"},
                {"path":"/not-opened/report-retry-3.json", "format":"ctrf"}
            ]
        })
    );
    assert_eq!(inspect(&path, None)["complete"], true);
    events[0].as_object_mut().unwrap().remove("binary-ids");
    write(&path, &events);
    issue(&inspect(&path, None), "missing admitted binary ids");
}

#[test]
fn malformed_records_preserve_only_the_transactional_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    for (key, invalid) in [
        ("v", json!(2)),
        ("seq", json!(5)),
        ("seq", json!(3.5)),
        ("time", json!("2026-02-30T01:02:03.004Z")),
        ("time", json!("2026-11-21T01:02:03Z")),
        ("elapsed-ms", json!(-1)),
        ("elapsed-ms", json!(1)),
        ("invocation-id", json!("other")),
        ("outcome", json!("unknown")),
        ("not-run", json!("1")),
        ("not-run", json!(u64::MAX)),
        ("passed", json!(2)),
    ] {
        let mut events = records();
        events[3][key] = invalid;
        write(&path, &events);
        let result = inspect(&path, None);
        issue(&result, "Malformed runner event at line 4");
        assert_eq!(result["outcome"], Value::Null);
        assert_eq!(
            result["reports"],
            json!([{"path":"/not-opened/report-retry-2.json", "format":"ctrf"}])
        );
    }
    let mut events = records();
    events[2]["reports"] = json!([
        {"path":"/must-not-leak", "format":"ctrf"}, {"path":"/bad", "format":17}
    ]);
    write(&path, &events);
    let result = inspect(&path, None);
    issue(&result, "Malformed runner event at line 3");
    assert_eq!(result["reports"], json!([]));
    assert_eq!(result["outcome"], Value::Null);
    for field in [
        "passed",
        "failed",
        "crashed",
        "launch-failed",
        "timed-out",
        "cancelled",
        "not-run",
    ] {
        let mut events = records();
        events[3].as_object_mut().unwrap().remove(field);
        write(&path, &events);
        issue(&inspect(&path, None), "Malformed runner event at line 4");
    }
    for index in [2, 3] {
        let mut events = records();
        events[index]["exit-code"] = json!(101);
        write(&path, &events);
        let result = inspect(&path, None);
        issue(&result, "outcome contradicts exit code");
        assert_eq!(result["outcome"], Value::Null);
        assert_eq!(
            result["reports"].as_array().unwrap().len(),
            if index == 2 { 0 } else { 1 }
        );
    }
}

#[test]
fn identity_order_and_membership_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    let mut cases = Vec::new();
    for (index, key, value) in [
        (1, "binary-id", json!("not-admitted")),
        (2, "binary-id", json!("other")),
        (2, "attempt", json!(2)),
        (2, "attempt", json!(1.5)),
        (2, "outcome", json!("not-run")),
        (0, "binary-ids", json!(["a", "a"])),
        (0, "binary-ids", json!(null)),
        (3, "not-run", json!(1)),
    ] {
        let mut events = records();
        events[index][key] = value;
        cases.push(events);
    }
    let mut reordered = records();
    reordered.swap(1, 2);
    reordered[1]["seq"] = json!(1);
    reordered[2]["seq"] = json!(2);
    cases.push(reordered);
    let mut wrong_order = records();
    wrong_order[0]["binary-ids"] = json!(["b", "a"]);
    cases.push(wrong_order);
    let mut overlapping = records();
    overlapping[0]["binary-ids"] = json!(["a", "b"]);
    overlapping.insert(2, overlapping[1].clone());
    overlapping[2]["binary-id"] = json!("b");
    overlapping[2]["seq"] = json!(2);
    overlapping[2]["elapsed-ms"] = json!(2);
    cases.push(overlapping);
    let mut active = records();
    active.remove(2);
    active[2]["seq"] = json!(2);
    cases.push(active);
    let mut duplicate = records();
    duplicate.insert(3, duplicate[1].clone());
    duplicate[3]["seq"] = json!(3);
    duplicate[3]["elapsed-ms"] = json!(3);
    cases.push(duplicate);
    let mut trailing = records();
    trailing.push(trailing[0].clone());
    cases.push(trailing);
    for events in cases {
        write(&path, &events);
        issue(&inspect(&path, None), "Malformed runner event");
    }
    write(&path, &records());
    let result = inspect(&path, Some("wrong"));
    issue(&result, "unexpected invocation identity");
    assert_eq!(result["invocation_id"], Value::Null);
}

#[test]
fn invocation_verdict_preserves_selection_order_and_runner_failure_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    let base = records();
    let mut events = vec![
        base[0].clone(),
        base[1].clone(),
        base[2].clone(),
        base[1].clone(),
        base[2].clone(),
        base[3].clone(),
    ];
    events[0]["binary-ids"] = json!(["a", "b"]);
    events[3]["binary-id"] = json!("b");
    events[4]["binary-id"] = json!("b");
    events[2]["outcome"] = json!("failed");
    events[4]["outcome"] = json!("failed");
    events[5]["outcome"] = json!("failed");
    events[5]["passed"] = json!(0);
    events[5]["failed"] = json!(2);
    for (first, second) in [(101, 7), (7, 101)] {
        events[2]["exit-code"] = json!(first);
        events[4]["exit-code"] = json!(second);
        events[5]["exit-code"] = json!(first);
        write(&path, &sequence(events.clone()));
        assert_eq!(inspect(&path, None)["complete"], true);
        for invalid in [second, 1, 0] {
            events[5]["exit-code"] = json!(invalid);
            write(&path, &sequence(events.clone()));
            let result = inspect(&path, None);
            issue(&result, "native verdict");
            assert_eq!(result["outcome"], Value::Null);
            assert_eq!(result["reports"], base[2]["reports"]);
        }
    }
    let mut events = records();
    // A cleanup failure can turn otherwise successful work into exit 1.
    events[3]["outcome"] = json!("failed");
    events[3]["exit-code"] = json!(1);
    write(&path, &events);
    assert_eq!(inspect(&path, None)["complete"], true);
    events[3]["exit-code"] = json!(7);
    write(&path, &events);
    issue(&inspect(&path, None), "native verdict");

    let mut events = records();
    events[2]["outcome"] = json!("crashed");
    events[2]["exit-code"] = Value::Null;
    events[2]["signal"] = json!(9);
    events[3]["outcome"] = json!("failed");
    events[3]["exit-code"] = json!(137);
    events[3]["passed"] = json!(0);
    events[3]["crashed"] = json!(1);
    write(&path, &events);
    assert_eq!(inspect(&path, None)["complete"], true);
    events[3]["exit-code"] = json!(9);
    write(&path, &events);
    issue(&inspect(&path, None), "native verdict");
}

#[test]
fn stops_are_sticky_across_binary_completion_and_block_every_admission_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    let base = records();
    for (kind, signal, outcome, code) in [
        ("deadline-expired", Value::Null, "timed-out", 124),
        ("cancel-requested", json!(15), "cancelled", 143),
    ] {
        let stop = json!({"event":kind, "signal":signal, "binary-id":null, "at-elapsed-ms":2});
        let mut valid = base.clone();
        valid.insert(2, stop.clone());
        valid[3]["outcome"] = json!(outcome);
        valid[3]["exit-code"] = Value::Null;
        valid[4]["outcome"] = json!(outcome);
        valid[4]["exit-code"] = json!(code);
        valid[4]["passed"] = json!(0);
        valid[4][outcome] = json!(1);
        write(&path, &sequence(valid));
        assert_eq!(inspect(&path, None)["complete"], true);

        for stop_while_running in [true, false] {
            let mut events = base.clone();
            events[0]["binary-ids"] = json!(["a", "b"]);
            events.insert(if stop_while_running { 2 } else { 3 }, stop.clone());
            events[4]["outcome"] = json!(outcome);
            events[4]["exit-code"] = json!(code);
            events[4]["not-run"] = json!(1);
            write(&path, &sequence(events.clone()));
            let result = inspect(&path, None);
            assert_eq!(result["outcome"], outcome, "{result}");
            assert_eq!(result["issues"].as_array().unwrap().len(), 1, "{result}");
            issue(&result, "1 selected binaries did not run");
            for admission in ["binary-started", "binary-launch-failed"] {
                let mut invalid = events.clone();
                invalid.insert(4, json!({"event":admission, "binary-id":"b", "attempt":1, "error":"launch failure"}));
                write(&path, &sequence(invalid));
                let result = inspect(&path, None);
                issue(&result, "after stop");
                assert_eq!(result["outcome"], Value::Null);
                assert_eq!(result["reports"], base[2]["reports"]);
            }
        }
    }
}

#[test]
fn cancellation_overrides_deadline_and_binary_failure_without_reopening_admission() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    let mut events = records();
    events[2]["outcome"] = json!("failed");
    events[2]["exit-code"] = json!(7);
    events.insert(
        3,
        json!({"event":"deadline-expired", "signal":null, "binary-id":null, "at-elapsed-ms":3}),
    );
    events.insert(
        4,
        json!({"event":"cancel-requested", "signal":2, "binary-id":null, "at-elapsed-ms":3}),
    );
    events[5]["outcome"] = json!("cancelled");
    events[5]["exit-code"] = json!(130);
    events[5]["passed"] = json!(0);
    events[5]["failed"] = json!(1);
    for reverse_stops in [false, true] {
        if reverse_stops {
            events.swap(3, 4);
        }
        write(&path, &sequence(events.clone()));
        assert_eq!(inspect(&path, None)["complete"], true);
        for (outcome, code) in [("failed", 7), ("timed-out", 124), ("cancelled", 143)] {
            let mut invalid = events.clone();
            invalid[5]["outcome"] = json!(outcome);
            invalid[5]["exit-code"] = json!(code);
            write(&path, &sequence(invalid));
            issue(&inspect(&path, None), "native verdict");
        }
    }
}

#[test]
fn missing_unreadable_invalid_utf8_malformed_and_truncated_are_successful_inspections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    issue(&inspect(&path, None), "Unreadable runner stream");
    issue(&inspect(dir.path(), None), "Unreadable runner stream");
    std::fs::write(&path, "").unwrap();
    issue(&inspect(&path, None), "Missing invocation-finished");
    for suffix in [b"{broken\n".as_slice(), b"\xff\n", b"{truncated", b"{}"] {
        write(&path, &records()[..3]);
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(suffix)
            .unwrap();
        let result = inspect(&path, None);
        assert_eq!(result["reports"].as_array().unwrap().len(), 1);
        issue(&result, "Missing invocation-finished");
    }
    write(&path, &records());
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"tail")
        .unwrap();
    let result = inspect(&path, None);
    issue(&result, "Truncated last line");
    assert_eq!(result["outcome"], "passed");
}

#[test]
fn report_coverage_and_skipped_or_unlaunchable_binaries_are_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    for (reports, message) in [(Value::Null, "reports=null"), (json!([]), "reports=[]")] {
        let mut events = records();
        events[2]["reports"] = reports;
        write(&path, &events);
        let result = inspect(&path, None);
        issue(&result, message);
        assert_eq!(result["outcome"], "passed");
    }
    let mut events = records();
    events[0]["binary-ids"] = json!(["a", "b"]);
    events[3]["not-run"] = json!(1);
    events[3]["outcome"] = json!("failed");
    events[3]["exit-code"] = json!(1);
    write(&path, &events);
    issue(&inspect(&path, None), "1 selected binaries did not run");
    let mut events = records();
    events.remove(2);
    events[1]["event"] = json!("binary-launch-failed");
    events[1]["error"] = json!("missing interpreter");
    events[2]["seq"] = json!(2);
    events[2]["outcome"] = json!("failed");
    events[2]["exit-code"] = json!(1);
    events[2]["passed"] = json!(0);
    events[2]["launch-failed"] = json!(1);
    write(&path, &events);
    let result = inspect(&path, None);
    issue(&result, "failed to launch");
    assert_eq!(result["outcome"], "failed");
    assert_eq!(result["reports"], json!([]));
}

#[test]
fn requested_format_is_coverage_per_binary_not_a_report_filter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    let mut events = records();
    events[0]["binary-ids"] = json!(["a", "b"]);
    events[2]["reports"] = json!([{"path":"/a.xml", "format":"junit"}]);
    let mut start = events[1].clone();
    start["binary-id"] = json!("b");
    let mut finish = events[2].clone();
    finish["binary-id"] = json!("b");
    finish["reports"] = json!([{"path":"/b.json", "format":"ctrf"}]);
    events.insert(3, start);
    events.insert(4, finish);
    for (i, event) in events.iter_mut().enumerate() {
        event["seq"] = json!(i);
        event["elapsed-ms"] = json!(i);
    }
    events[5]["passed"] = json!(2);
    write(&path, &events);
    assert_eq!(inspect(&path, None)["complete"], true);
    let result = stdout_json(
        cargo_test_r()
            .args(["inspect-events", "--events"])
            .arg(&path)
            .args(["--report-format", "ctrf"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        result["issues"],
        json!(["No ctrf report identified for binary a"])
    );
    assert_eq!(result["complete"], false);
    assert_eq!(result["outcome"], "passed");
    assert_eq!(
        result["reports"],
        json!([
            {"path":"/a.xml", "format":"junit"}, {"path":"/b.json", "format":"ctrf"}
        ])
    );
}

#[test]
fn real_runner_output_is_inspected_without_opening_reports() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = common::rustc_fixture(
        r#"
fn main() {
    std::fs::write(std::env::var("TEST_R_REPORT_INDEX").unwrap(),
        "{\"path\":\"/nonexistent/actual-retry-report.json\",\"format\":\"ctrf\"}\n").unwrap();
}
"#,
    );
    let selection = dir.path().join("selection.json");
    common::write_json(&selection, &common::selection(&[fixture], dir.path()));
    let events = dir.path().join("events");
    common::success(
        cargo_test_r()
            .args(["run", "--selection"])
            .arg(selection)
            .arg("--events")
            .arg(&events)
            .args(["--invocation-id", "real"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        inspect(&events, Some("real")),
        json!({
            "v":1, "invocation_id":"real", "complete":true, "outcome":"passed", "issues":[],
            "reports":[{"path":"/nonexistent/actual-retry-report.json", "format":"ctrf"}]
        })
    );
}

#[test]
fn real_harness_retry_report_identity_is_not_a_binary_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let build = std::process::Command::new("cargo")
        .current_dir(root)
        .args([
            "test",
            "-p",
            "test-r",
            "--test",
            "retry_failed_fixture",
            "--no-run",
            "--message-format=json",
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let executable = String::from_utf8(build.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|v| {
            (v["target"]["name"] == "retry_failed_fixture")
                .then(|| v["executable"].as_str().map(std::path::PathBuf::from))
                .flatten()
        })
        .unwrap();
    let selection = dir.path().join("selection.json");
    common::write_json(&selection, &common::selection(&[executable], dir.path()));
    let events = dir.path().join("events");
    common::success(
        cargo_test_r()
            .current_dir(dir.path())
            .args(["run", "--selection"])
            .arg(&selection)
            .arg("--events")
            .arg(&events)
            .env("TEST_R_RETRY_FAILED_DIR", dir.path().join("calls"))
            .args([
                "--",
                "panics_once",
                "--include-ignored",
                "--retry-failed",
                "1",
                "--format",
                "ctrf",
                "--logfile",
                "retry.json",
            ])
            .output()
            .unwrap(),
    );
    let stream: Vec<Value> = std::fs::read_to_string(&events)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(stream[1]["attempt"], 1);
    assert_eq!(stream[2]["attempt"], 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls/panics_once")).unwrap(),
        "xx"
    );
    let result = inspect(&events, None);
    assert_eq!(result["complete"], true, "{result}");
    assert_eq!(result["reports"], stream[2]["reports"]);
    let report = Path::new(result["reports"][0]["path"].as_str().unwrap());
    assert!(
        report
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("retry-")
    );
    // Inspection uses the recorded identity, not report existence or content.
    std::fs::remove_file(report).unwrap();
    assert_eq!(inspect(&events, None), result);
}
