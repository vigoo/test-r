use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn fixture(dir: &Path) -> PathBuf {
    let source = dir.join("fixture.rs");
    std::fs::write(&source, r#"
            fn main() {
                let exe = std::env::current_exe().unwrap();
                let name = exe.file_stem().unwrap().to_str().unwrap();
                if let Ok(path) = std::env::var("TRACE") {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
                    writeln!(file, "{}|{}|{:?}", name, std::env::current_dir().unwrap().canonicalize().unwrap().display(), std::env::args().skip(1).collect::<Vec<_>>()).unwrap();
                }
                if name == "fail" { std::process::exit(7); }
            }
        "#).unwrap();
    let exe = dir.join(format!("fixture{}", std::env::consts::EXE_SUFFIX));
    assert!(
        Command::new("rustc")
            .arg(source)
            .arg("-o")
            .arg(&exe)
            .status()
            .unwrap()
            .success()
    );
    exe
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
        let alpha = dir.path().join("a/same");
        let decoy = dir.path().join("b/same");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&decoy).unwrap();
        let target = dir.path().join("redirected-output");
        std::fs::create_dir_all(&target).unwrap();
        let executable = target.join(format!("ok{}", std::env::consts::EXE_SUFFIX));
        let fixture = fixture(dir.path());
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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-test-r"));
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

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(output: Output, message: &str) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
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
        Sha256::digest(std::fs::read(&w.metadata).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    assert_eq!(selection["binaries"][0]["package-name"], "alpha");
    assert_eq!(
        selection["binaries"][0]["working-directory"],
        w.dir.path().join("a/same").to_str().unwrap()
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
    let package = w.dir.path().join("space here/legacy");
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
        w.dir.path().join("a/same").canonicalize().unwrap().display()
    )));
    assert!(lines[1].contains(&format!(
            "|{}|",
            w.dir
                .path()
                .join("b/same")
                .canonicalize()
                .unwrap()
                .display()
        )));
    failure(
        w.command("run")
            .arg("--selection")
            .arg(&path)
            .args(["--lib"])
            .output()
            .unwrap(),
        "runs exactly",
    );
    let fail = w
        .dir
        .path()
        .join(format!("fail{}", std::env::consts::EXE_SUFFIX));
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
    failure(
        w.command("run")
            .arg("--binaries-metadata")
            .arg(&w.metadata)
            .args(["--no-fail-fast"])
            .output()
            .unwrap(),
        "--no-fail-fast is not supported",
    );
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
            url::Url::from_directory_path(old_workspace.join("a/same"))
                .unwrap()
                .as_str()
                .trim_end_matches('/')
        ));
        binary["binary-path"] =
            json!(old_target.join(format!("debug/deps/ok{}", std::env::consts::EXE_SUFFIX)));
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
        selection["binaries"][0]["executable"],
        exe.to_str().unwrap()
    );
    assert_eq!(
        selection["binaries"][0]["working-directory"],
        w.dir.path().join("a/same").to_str().unwrap()
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
