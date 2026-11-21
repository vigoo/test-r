mod common;

use common::{cargo_test_r, exe, failure, stdout_json as success};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn fixture() -> PathBuf {
    common::rustc_fixture(
        r#"
            fn main() {
                let exe = std::env::current_exe().unwrap();
                let name = exe.file_stem().unwrap().to_str().unwrap();
                if let Ok(path) = std::env::var("TRACE") {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
                    writeln!(file, "{}|{}|{:?}", name, std::env::current_dir().unwrap().canonicalize().unwrap().display(), std::env::args().skip(1).collect::<Vec<_>>()).unwrap();
                }
                if let Ok(path) = std::env::var("REPORTS") {
                    std::fs::write(std::path::Path::new(&path).join(format!("{name}.json")), format!("{{\"binary\":\"{name}\"}}")).unwrap();
                }
                if name == "fail" { std::process::exit(7); }
                if name == "fail-later" { std::process::exit(19); }
                if name == "crash" { std::process::abort(); }
            }
        "#,
    )
}

struct Workspace {
    dir: TempDir,
    metadata: PathBuf,
    document: Value,
    fixture: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Both directories have the same last component, neither is the package name.
        let alpha = dir.path().join("a").join("same");
        let decoy = dir.path().join("b").join("same");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&decoy).unwrap();
        let target = dir.path().join("redirected-output");
        std::fs::create_dir_all(&target).unwrap();
        let executable = target.join(exe("ok"));
        // A private copy: tests pass it to the runner and copy it further.
        let fixture = dir.path().join(exe("fixture"));
        std::fs::copy(self::fixture(), &fixture).unwrap();
        std::fs::copy(&fixture, &executable).unwrap();
        let mut binaries = serde_json::Map::new();
        for (id, name, kind, package, path) in [
            ("alpha::integration", "integration", "test", "alpha", &alpha),
            ("alpha", "alpha_lib", "lib", "alpha", &alpha),
            ("alpha::bin/tool", "tool", "bin", "alpha", &alpha),
            ("decoy::integration", "integration", "test", "decoy", &decoy),
            ("alpha::bench/speed", "speed", "bench", "alpha", &alpha),
        ] {
            let url = url::Url::from_directory_path(path).unwrap();
            binaries.insert(id.into(), json!({
                "binary-id": id, "binary-name": name, "kind": kind,
                "package-id": format!("path+{}#{}@0.1.0", url.as_str().trim_end_matches('/'), package),
                "binary-path": executable, "build-platform": "target"
            }));
        }
        let document = json!({ "rust-build-meta": {
            "target-directory": target, "base-output-directories": ["debug"],
            "non-test-binaries": {}, "linked-paths": [], "target-platform": null
        }, "rust-binaries": binaries });
        let metadata = dir.path().join("binaries.json");
        let result = Self {
            dir,
            metadata,
            document,
            fixture,
        };
        result.save();
        result
    }

    fn save(&self) {
        std::fs::write(&self.metadata, self.document.to_string()).unwrap();
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut cmd = cargo_test_r();
        cmd.current_dir(self.dir.path())
            .env("CARGO", self.dir.path().join("no-cargo"))
            .env("TRACE", self.dir.path().join("trace"))
            .arg("test-r")
            .arg(subcommand);
        cmd
    }

    fn list(&self, args: &[&str]) -> Output {
        self.command("list")
            .arg("--binaries-metadata")
            .arg(&self.metadata)
            .args(args)
            .output()
            .unwrap()
    }
}

fn ids(selection: &Value) -> Vec<&str> {
    selection["binaries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["binary-id"].as_str().unwrap())
        .collect()
}

#[test]
fn inventory_filters_identity_order_and_no_execution() {
    let w = Workspace::new();
    let selection = success(w.list(&[]));
    assert_eq!(selection["format"], "cargo-test-r-selection");
    assert_eq!(selection["version"], 1);
    assert_eq!(selection["inputs"][0]["kind"], "nextest-binaries");
    assert_eq!(
        ids(&selection),
        [
            "alpha",
            "alpha::bench/speed",
            "alpha::bin/tool",
            "alpha::integration",
            "decoy::integration"
        ]
    );
    assert_eq!(
        selection["inputs"][0]["sha256"],
        common::sha256_hex(&std::fs::read(&w.metadata).unwrap())
    );
    assert_eq!(selection["binaries"][0]["package-name"], "alpha");
    assert_eq!(
        selection["binaries"][0]["working-directory"],
        w.dir.path().join("a").join("same").to_str().unwrap()
    );
    assert_eq!(
        ids(&success(w.list(&["-p", "a*", "--test", "integ*"]))),
        ["alpha::integration"]
    );
    assert_eq!(
        ids(&success(w.list(&["--exclude", "dec*", "--lib"]))),
        ["alpha"]
    );
    assert_eq!(ids(&success(w.list(&["--bins"]))), ["alpha::bin/tool"]);
    assert_eq!(
        ids(&success(w.list(&[
            "--lib",
            "--test",
            "integration",
            "-p",
            "alpha"
        ]))),
        ["alpha", "alpha::integration"]
    );
    assert_eq!(
        ids(&success(w.list(&["--tests", "-p", "alpha"]))),
        ["alpha", "alpha::bin/tool", "alpha::integration"]
    );
    assert_eq!(
        ids(&success(w.list(&[
            "--binary-id",
            "decoy::integration",
            "--binary-id",
            "alpha"
        ]))),
        ["alpha", "decoy::integration"]
    );
    assert!(!w.dir.path().join("trace").exists());
    // Stable bytes, not just stable parsed JSON.
    assert_eq!(w.list(&[]).stdout, w.list(&[]).stdout);
}

#[test]
fn legacy_package_id_and_percent_encoded_directory() {
    let mut w = Workspace::new();
    let package = w.dir.path().join("space here").join("legacy");
    std::fs::create_dir_all(&package).unwrap();
    w.document["rust-binaries"]["alpha"]["package-id"] = json!(format!(
        "path+{}#0.1.0",
        url::Url::from_directory_path(&package)
            .unwrap()
            .as_str()
            .trim_end_matches('/')
    ));
    w.save();
    let selection = success(w.list(&["-p", "legacy"]));
    assert_eq!(ids(&selection), ["alpha"]);
    assert_eq!(
        selection["binaries"][0]["working-directory"],
        package.to_str().unwrap()
    );
}

#[test]
fn repeated_inputs_merge_but_conflicting_variants_fail() {
    let mut w = Workspace::new();
    let same = w.metadata.to_str().unwrap();
    let selection = success(w.list(&["--binaries-metadata", same]));
    assert_eq!(ids(&selection).len(), 5);
    assert_eq!(selection["inputs"].as_array().unwrap().len(), 2);
    let first_variant = selection["binaries"][0]["build-variant"].clone();
    let other = w.dir.path().join("other.json");
    std::fs::copy(&w.metadata, &other).unwrap();
    for (field, value) in [
        ("base-output-directories", json!(["release"])),
        ("target-platform", json!("aarch64-unknown-linux-gnu")),
        (
            "target-directory",
            json!(w.dir.path().join("different-target")),
        ),
    ] {
        let old = w.document["rust-build-meta"][field].clone();
        w.document["rust-build-meta"][field] = value;
        w.save();
        assert_ne!(
            success(w.list(&[]))["binaries"][0]["build-variant"],
            first_variant
        );
        failure(
            w.list(&["--binaries-metadata", other.to_str().unwrap()]),
            "Conflicting entries for binary id alpha",
        );
        w.document["rust-build-meta"][field] = old;
    }
    w.document["rust-binaries"]["alpha"]["binary-path"] = json!(w.fixture);
    w.save();
    failure(
        w.list(&["--binaries-metadata", other.to_str().unwrap()]),
        "Conflicting entries for binary id alpha",
    );
}

#[test]
fn metadata_errors_are_fail_closed() {
    let mut w = Workspace::new();
    failure(w.list(&["-p", "absent"]), "No test binaries match");
    w.document["rust-binaries"]["alpha"]["binary-path"] = json!(w.dir.path().join("gone"));
    w.save();
    for subcommand in ["list", "run"] {
        failure(
            w.command(subcommand)
                .arg("--binaries-metadata")
                .arg(&w.metadata)
                .output()
                .unwrap(),
            "does not exist",
        );
    }
    w.document["rust-binaries"]["alpha"]["package-id"] =
        json!("registry+https://example.org#index@1.0");
    w.save();
    failure(w.list(&[]), "Unsupported package id");
    std::fs::write(&w.metadata, "{}").unwrap();
    failure(w.list(&[]), "Invalid binaries metadata");
    std::fs::remove_file(&w.metadata).unwrap();
    failure(w.list(&[]), "Failed to read binaries metadata");
    assert!(!w.dir.path().join("trace").exists());
}

#[test]
fn non_executable_artifacts_are_rejected() {
    let mut w = Workspace::new();
    let exe = w.dir.path().join("not-executable.txt");
    std::fs::write(&exe, "not an executable").unwrap();
    w.document["rust-binaries"]["alpha"]["binary-path"] = json!(exe);
    w.save();
    failure(w.list(&[]), "not executable");
    failure(
        w.command("run")
            .arg("--binaries-metadata")
            .arg(&w.metadata)
            .output()
            .unwrap(),
        "not executable",
    );
}

#[test]
fn saved_selection_runs_exactly_it_in_order_with_arguments_and_fail_fast() {
    let mut w = Workspace::new();
    let selected = success(w.list(&["--binary-id", "decoy::integration", "--binary-id", "alpha"]));
    let path = w.dir.path().join("selection.json");
    std::fs::write(&path, selected.to_string()).unwrap();
    // The saved file does not consult its metadata inputs again.
    std::fs::remove_file(&w.metadata).unwrap();
    let output = w
        .command("run")
        .arg("--selection")
        .arg(&path)
        .args(["name", "--", "--exact"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    let trace = std::fs::read_to_string(w.dir.path().join("trace")).unwrap();
    let lines: Vec<_> = trace.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains(&format!(
        "|{}|[\"name\", \"--exact\"]",
        w.dir.path().join("a").join("same").canonicalize().unwrap().display()
    )));
    assert!(lines[1].contains(&format!(
            "|{}|",
            w.dir
                .path()
                .join("b").join("same")
                .canonicalize()
                .unwrap()
                .display()
        )));
    failure(
        w.command("run")
            .arg("--selection")
            .arg(&path)
            .args(["--binary-id", "absent"])
            .output()
            .unwrap(),
        "No test binaries match",
    );
    let fail = w.dir.path().join(exe("fail"));
    std::fs::copy(&w.fixture, &fail).unwrap();
    w.document["rust-binaries"]["alpha"]["binary-path"] = json!(fail);
    w.save();
    std::fs::remove_file(w.dir.path().join("trace")).unwrap();
    let output = w
        .command("run")
        .arg("--binaries-metadata")
        .arg(&w.metadata)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        std::fs::read_to_string(w.dir.path().join("trace"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let output = w
        .command("run")
        .arg("--binaries-metadata")
        .arg(&w.metadata)
        .arg("--no-fail-fast")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(w.dir.path().join("trace"))
            .unwrap()
            .lines()
            .count(),
        6
    );
}

#[test]
fn saved_selection_filters_and_count_assertions_do_not_read_inputs_or_rewrite_manifest() {
    let w = Workspace::new();
    let selected = success(w.list(&[]));
    let path = w.dir.path().join("selection.json");
    let original = selected.to_string();
    std::fs::write(&path, &original).unwrap();
    std::fs::remove_file(&w.metadata).unwrap();
    for flags in [
        vec!["-p", "alpha", "--test", "integ*"],
        vec!["--exclude", "decoy", "--bins"],
        vec!["--binary-id", "alpha", "--lib"],
    ] {
        let result = w
            .command("run")
            .arg("--selection")
            .arg(&path)
            .args(flags)
            .args(["--expect-count", "1"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(w.dir.path().join("trace"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    failure(
        w.command("run")
            .arg("--selection")
            .arg(&path)
            .args(["--tests", "--expect-count", "1"])
            .output()
            .unwrap(),
        "Expected exactly 1 test binaries, selected 4",
    );
    for flag in ["--target-dir", "--cargo-metadata"] {
        failure(
            w.command("run")
                .arg("--selection")
                .arg(&path)
                .args([flag, "unused"])
                .output()
                .unwrap(),
            "cannot be combined",
        );
    }
}

#[test]
fn executable_path_output_is_exact_unambiguous_and_does_not_execute() {
    let w = Workspace::new();
    let result = w.list(&[
        "--binary-id",
        "alpha",
        "--expect-count",
        "1",
        "--output",
        "executable-path",
    ]);
    assert!(result.status.success(), "{result:?}");
    let terminated = w.list(&[
        "--binary-id",
        "alpha",
        "--expect-count",
        "1",
        "--output",
        "executable-path",
        "--",
    ]);
    assert!(terminated.status.success(), "{terminated:?}");
    assert_eq!(terminated.stdout, result.stdout);
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        format!(
            "{}\n",
            w.document["rust-binaries"]["alpha"]["binary-path"]
                .as_str()
                .unwrap()
        )
    );
    failure(
        w.list(&["--output", "executable-path"]),
        "requires exactly one",
    );
    failure(
        w.list(&["--lib", "--expect-count", "2"]),
        "Expected exactly 2 test binaries, selected 1",
    );
    assert!(!w.dir.path().join("trace").exists());
}

#[test]
fn selection_format_and_artifacts_are_validated_before_running() {
    let w = Workspace::new();
    let mut selection = success(w.list(&[]));
    let path = w.dir.path().join("selection.json");
    for (field, value, message) in [
        ("version", json!(2), "Unsupported selection"),
        ("format", json!("other"), "Unsupported selection"),
        ("binaries", json!([]), "No test binaries match"),
    ] {
        let old = selection[field].clone();
        selection[field] = value;
        std::fs::write(&path, selection.to_string()).unwrap();
        failure(
            w.command("run")
                .arg("--selection")
                .arg(&path)
                .output()
                .unwrap(),
            message,
        );
        selection[field] = old;
    }
    // Only snapshot reads inputs, so run accepts kinds from newer versions.
    selection["inputs"][0]["kind"] = json!("future-thing");
    std::fs::write(&path, selection.to_string()).unwrap();
    let output = w
        .command("run")
        .arg("--selection")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let trace = w.dir.path().join("trace");
    assert_eq!(std::fs::read_to_string(&trace).unwrap().lines().count(), 5);
    std::fs::remove_file(trace).unwrap();
    selection["binaries"][4]["executable"] = json!(w.dir.path().join("missing"));
    std::fs::write(&path, selection.to_string()).unwrap();
    failure(
        w.command("run")
            .arg("--selection")
            .arg(&path)
            .output()
            .unwrap(),
        "does not exist",
    );
    assert!(!w.dir.path().join("trace").exists());
}

#[test]
fn extracted_archive_relocates_redirected_target_and_workspace() {
    let w = Workspace::new();
    std::fs::write(w.dir.path().join("Cargo.lock"), "").unwrap();
    let archive = w.dir.path().join("archive-target/nextest-archive");
    let meta = archive.join("target/nextest");
    std::fs::create_dir_all(&meta).unwrap();
    let mut document = w.document.clone();
    let old_workspace = w.dir.path().join("old-workspace");
    let old_target = w.dir.path().join("external-target");
    for binary in document["rust-binaries"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        binary["package-id"] = json!(format!(
            "path+{}#alpha@0.1.0",
            url::Url::from_directory_path(old_workspace.join("a").join("same"))
                .unwrap()
                .as_str()
                .trim_end_matches('/')
        ));
        binary["binary-path"] = json!(old_target.join(format!("debug/deps/{}", exe("ok"))));
    }
    document["rust-build-meta"]["target-directory"] = json!(old_target);
    std::fs::write(meta.join("binaries-metadata.json"), document.to_string()).unwrap();
    std::fs::write(
        meta.join("cargo-metadata.json"),
        json!({"workspace_root": old_workspace}).to_string(),
    )
    .unwrap();
    let exe = archive.join(format!(
        "target/debug/deps/ok{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::copy(&w.fixture, &exe).unwrap();
    let selection = success(
        w.command("list")
            .args(["--target-dir", "archive-target", "--lib"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        Path::new(selection["binaries"][0]["executable"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        exe.canonicalize().unwrap()
    );
    assert_eq!(
        Path::new(
            selection["binaries"][0]["working-directory"]
                .as_str()
                .unwrap()
        )
        .canonicalize()
        .unwrap(),
        w.dir.path().join("a").join("same").canonicalize().unwrap()
    );
    assert!(!w.dir.path().join("trace").exists());
    let output = w
        .command("run")
        .args(["--target-dir", "archive-target", "--lib"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(w.dir.path().join("trace"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    std::fs::remove_file(exe).unwrap();
    failure(
        w.command("list")
            .args(["--target-dir", "archive-target"])
            .output()
            .unwrap(),
        "does not exist",
    );
}

// Exercise the same execution contract through every prebuilt input path.
#[derive(Clone, Copy, Debug)]
enum Input {
    Metadata,
    Archive,
    Selection,
}

fn outcome_run(input: Input, names: [&str; 3], no_fail_fast: bool, code: i32) -> Output {
    let mut w = Workspace::new();
    let target = w.dir.path().join("redirected-output");
    let ids = ["alpha", "alpha::integration", "decoy::integration"];
    w.document["rust-binaries"]
        .as_object_mut()
        .unwrap()
        .retain(|id, _| ids.contains(&id.as_str()));
    for (id, name) in ids.iter().zip(names) {
        let exe = target.join(exe(name));
        if name == "launch" {
            // Pass artifact validation but let the OS reject the image at spawn.
            #[cfg(windows)]
            std::fs::write(&exe, "not an executable image").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Darwin executes plain text via sh; a missing interpreter
                // reliably fails spawn with ENOENT on both Darwin and Linux.
                std::fs::write(&exe, "#!/nonexistent/interpreter\n").unwrap();
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        } else {
            std::fs::copy(&w.fixture, &exe).unwrap();
        }
        w.document["rust-binaries"][id]["binary-path"] = json!(exe);
    }
    w.save();
    let mut cmd = w.command("run");
    match input {
        Input::Metadata => {
            cmd.arg("--binaries-metadata").arg(&w.metadata);
        }
        Input::Selection => {
            let selection = success(w.list(&[]));
            let path = w.dir.path().join("selection.json");
            std::fs::write(&path, selection.to_string()).unwrap();
            cmd.arg("--selection").arg(path);
        }
        Input::Archive => {
            std::fs::write(w.dir.path().join("Cargo.lock"), "").unwrap();
            let archive = w.dir.path().join("archive-target/nextest-archive");
            let meta = archive.join("target/nextest");
            std::fs::create_dir_all(&meta).unwrap();
            for name in names {
                let filename = exe(name);
                std::fs::copy(
                    target.join(&filename),
                    archive.join("target").join(filename),
                )
                .unwrap();
            }
            std::fs::write(meta.join("binaries-metadata.json"), w.document.to_string()).unwrap();
            std::fs::write(
                meta.join("cargo-metadata.json"),
                json!({"workspace_root": w.dir.path()}).to_string(),
            )
            .unwrap();
            cmd.args(["--target-dir", "archive-target"]);
        }
    }
    let reports = w.dir.path().join("reports");
    std::fs::create_dir(&reports).unwrap();
    cmd.env("REPORTS", &reports);
    if no_fail_fast {
        cmd.arg("--no-fail-fast");
    }
    let output = cmd.args(["filter", "--", "--exact"]).output().unwrap();
    assert_eq!(output.status.code(), Some(code), "{input:?}: {output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let trace = std::fs::read_to_string(w.dir.path().join("trace")).unwrap_or_default();
    let lines: Vec<_> = trace.lines().collect();
    let mut executed = 0;
    let mut stopped = false;
    for (index, name) in names.iter().enumerate() {
        let ran = !stopped && *name != "launch";
        let report = reports.join(format!("{name}.json"));
        assert_eq!(report.exists(), ran, "{input:?}: {name}: {output:?}");
        if ran {
            assert_eq!(
                serde_json::from_slice::<Value>(&std::fs::read(report).unwrap()).unwrap(),
                json!({"binary": name})
            );
            let cwd = w
                .dir
                .path()
                .join(if index == 2 { "b/same" } else { "a/same" })
                .canonicalize()
                .unwrap();
            assert_eq!(
                lines[executed],
                format!("{name}|{}|[\"filter\", \"--exact\"]", cwd.display())
            );
            executed += 1;
        }
        if *name != "ok" && *name != "later" && *name != "last" && !no_fail_fast {
            stopped = true;
        }
    }
    assert_eq!(lines.len(), executed, "{input:?}: {trace}");
    output
}

#[test]
fn prebuilt_outcomes_fail_fast_continue_and_preserve_reports() {
    for input in [Input::Metadata, Input::Archive, Input::Selection] {
        let default = outcome_run(input, ["fail", "later", "last"], false, 7);
        assert!(
            String::from_utf8_lossy(&default.stderr)
                .contains("Test binary alpha: failed with exit code 7")
        );
        outcome_run(input, ["fail", "later", "last"], true, 7);
        for no_fail_fast in [false, true] {
            let passed = outcome_run(input, ["ok", "later", "last"], no_fail_fast, 0);
            assert!(passed.stderr.is_empty(), "{passed:?}");
        }
        // A passing prefix must not hide the first failure, nor may a later code replace it.
        let failed = outcome_run(input, ["ok", "fail-later", "fail"], true, 19);
        let stderr = String::from_utf8_lossy(&failed.stderr);
        assert!(
            stderr.contains("Test binary alpha::integration: failed with exit code 19"),
            "{failed:?}"
        );
        assert!(
            stderr.contains("Test binary decoy::integration: failed with exit code 7"),
            "{failed:?}"
        );
        // Also rule out choosing the largest code rather than selection order.
        outcome_run(input, ["fail", "fail-later", "last"], true, 7);
        for no_fail_fast in [false, true] {
            let launch = outcome_run(input, ["launch", "later", "last"], no_fail_fast, 1);
            assert!(
                String::from_utf8_lossy(&launch.stderr)
                    .contains("Test binary alpha: launch failed"),
                "{launch:?}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn prebuilt_crash_reports_signal_and_continues_only_when_requested() {
    for input in [Input::Metadata, Input::Archive, Input::Selection] {
        for no_fail_fast in [false, true] {
            let output = outcome_run(input, ["crash", "later", "last"], no_fail_fast, 134);
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("Test binary alpha: crashed with signal 6"),
                "{output:?}"
            );
        }
    }
}

#[test]
fn cargo_fallback_forwards_no_fail_fast_and_preserves_exit_code() {
    let w = Workspace::new();
    std::fs::write(w.dir.path().join("Cargo.lock"), "").unwrap();
    let cargo = w.dir.path().join(exe("fail"));
    std::fs::copy(&w.fixture, &cargo).unwrap();
    let output = w
        .command("run")
        .env("CARGO", cargo)
        .args(["--no-fail-fast", "filter", "--", "--exact"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let trace = std::fs::read_to_string(w.dir.path().join("trace")).unwrap();
    assert_eq!(trace.lines().count(), 1);
    assert!(
        trace.ends_with("|[\"test\", \"--no-fail-fast\", \"filter\", \"--\", \"--exact\"]\n"),
        "{trace}"
    );
}
