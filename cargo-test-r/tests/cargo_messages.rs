use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Workspace {
    dir: tempfile::TempDir,
    messages: PathBuf,
    target: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers=['a/same','b/same']\nresolver='3'\n",
        )
        .unwrap();
        for (directory, package) in [("a/same", "alpha"), ("b/same", "beta")] {
            let package_dir = root.join(directory);
            std::fs::create_dir_all(package_dir.join("src")).unwrap();
            std::fs::create_dir_all(package_dir.join("tests")).unwrap();
            std::fs::write(package_dir.join("Cargo.toml"), format!("[package]\nname='{package}'\nversion='0.1.0'\nedition='2024'\n[features]\nextra=[]\n")).unwrap();
            for (file, label) in [("src/lib.rs", "lib"), ("tests/shared.rs", "shared")] {
                std::fs::write(package_dir.join(file), test_source(package, label)).unwrap();
            }
            if package == "alpha" {
                std::fs::create_dir_all(package_dir.join("src/bin")).unwrap();
                std::fs::write(
                    package_dir.join("src/bin/tool.rs"),
                    format!("fn main() {{}}\n{}", test_source(package, "tool")),
                )
                .unwrap();
            }
        }
        let messages = root.join("messages.jsonl");
        let target = root.join("redirected output");
        let workspace = Self {
            dir,
            messages,
            target,
        };
        workspace.build(&workspace.messages, false);
        workspace
    }

    fn build(&self, path: &Path, feature: bool) {
        let mut command = Command::new(env!("CARGO"));
        command
            .current_dir(self.dir.path())
            .args([
                "test",
                "--no-run",
                "--message-format",
                "json",
                "--offline",
                "--workspace",
                "--target-dir",
            ])
            .arg(&self.target);
        if feature {
            command.args(["--features", "alpha/extra"]);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(path, output.stdout).unwrap();
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-test-r"));
        command
            .current_dir(self.dir.path())
            .env("CARGO", "must-not-invoke-cargo")
            .env("MARKERS", self.dir.path().join("markers"))
            .arg(subcommand);
        command
    }

    fn list(&self, path: &Path) -> Command {
        let mut command = self.command("list");
        command.arg("--cargo-messages").arg(path);
        command
    }
}

fn test_source(package: &str, label: &str) -> String {
    format!(
        r#"
        #[test] fn passes() {{
            if let Ok(root) = std::env::var("MARKERS") {{
                std::fs::create_dir_all(&root).unwrap();
                std::fs::write(std::path::Path::new(&root).join("{package}-{label}"), std::env::current_dir().unwrap().to_str().unwrap()).unwrap();
            }}
        }}
    "#
    )
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(output: Output, expected: &str) {
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn real_cargo_inventory_duplicates_filters_and_saved_execution() {
    let w = Workspace::new();
    let selection = success(w.list(&w.messages).output().unwrap());
    let ids: Vec<_> = selection["binaries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["binary-id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "alpha",
            "alpha::bin/tool",
            "alpha::shared",
            "beta",
            "beta::shared"
        ]
    );
    assert_eq!(selection["version"], 1);
    assert_eq!(selection["inputs"][0]["kind"], "cargo-messages");
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(std::fs::read(&w.messages).unwrap());
    let expected: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(selection["inputs"][0]["sha256"], expected);
    for binary in selection["binaries"].as_array().unwrap() {
        assert!(Path::new(binary["executable"].as_str().unwrap()).starts_with(&w.target));
        let package = binary["package-name"].as_str().unwrap();
        assert_eq!(
            binary["working-directory"],
            w.dir
                .path()
                .join(if package == "alpha" {
                    "a/same"
                } else {
                    "b/same"
                })
                .to_str()
                .unwrap()
        );
    }
    assert!(!w.dir.path().join("markers").exists());
    let duplicate = w.dir.path().join("duplicate.jsonl");
    let bytes = std::fs::read(&w.messages).unwrap();
    std::fs::write(&duplicate, [bytes.as_slice(), bytes.as_slice()].concat()).unwrap();
    let repeated = success(
        w.list(&duplicate)
            .arg("--cargo-messages")
            .arg(&w.messages)
            .output()
            .unwrap(),
    );
    assert_eq!(selection["binaries"], repeated["binaries"]);
    let fresh = w.dir.path().join("fresh.jsonl");
    w.build(&fresh, false);
    let cached = success(
        w.list(&w.messages)
            .arg("--cargo-messages")
            .arg(&fresh)
            .output()
            .unwrap(),
    );
    assert_eq!(selection["binaries"], cached["binaries"]);
    for (args, expected) in [
        (vec!["-p", "alpha", "--lib"], vec!["alpha"]),
        (vec!["--bins"], vec!["alpha::bin/tool"]),
        (
            vec!["--test", "shared", "--exclude", "alpha"],
            vec!["beta::shared"],
        ),
        (vec!["--binary-id", "alpha::shared"], vec!["alpha::shared"]),
    ] {
        let filtered = success(w.list(&w.messages).args(args).output().unwrap());
        assert_eq!(
            filtered["binaries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["binary-id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
    }
    let saved = w.dir.path().join("selection.json");
    std::fs::write(&saved, selection.to_string()).unwrap();
    let run = w
        .command("run")
        .arg("--selection")
        .arg(&saved)
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    assert_eq!(
        std::fs::read_dir(w.dir.path().join("markers"))
            .unwrap()
            .count(),
        5
    );
    for (marker, directory) in [
        ("alpha-lib", "a/same"),
        ("alpha-tool", "a/same"),
        ("alpha-shared", "a/same"),
        ("beta-lib", "b/same"),
        ("beta-shared", "b/same"),
    ] {
        assert_eq!(
            std::fs::read_to_string(w.dir.path().join("markers").join(marker)).unwrap(),
            w.dir.path().join(directory).to_str().unwrap()
        );
    }
    let direct = w
        .command("run")
        .arg("--cargo-messages")
        .arg(&w.messages)
        .arg("--bins")
        .output()
        .unwrap();
    assert!(direct.status.success(), "{direct:?}");
    let conflict = w
        .command("run")
        .arg("--selection")
        .arg(&saved)
        .arg("--cargo-messages")
        .arg(&w.messages)
        .output()
        .unwrap();
    failure(conflict, "cannot be used with");
    #[cfg(target_os = "linux")]
    failure(
        w.command("snapshot")
            .arg("--selection")
            .arg(&saved)
            .arg("--out")
            .arg(w.dir.path().join("snapshot"))
            .output()
            .unwrap(),
        "snapshot does not support Cargo-message inputs",
    );
}

#[test]
fn cargo_variants_malformed_missing_and_mixed_inputs_fail_closed() {
    let w = Workspace::new();
    let featured = w.dir.path().join("featured.jsonl");
    w.build(&featured, true);
    let normal = success(
        w.list(&w.messages)
            .arg("--binary-id")
            .arg("alpha")
            .output()
            .unwrap(),
    );
    let feature = success(
        w.list(&featured)
            .arg("--binary-id")
            .arg("alpha")
            .output()
            .unwrap(),
    );
    assert_ne!(
        normal["binaries"][0]["build-variant"],
        feature["binaries"][0]["build-variant"]
    );
    let output = w
        .list(&w.messages)
        .arg("--cargo-messages")
        .arg(&featured)
        .args(["--binary-id", "alpha"])
        .output()
        .unwrap();
    let diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
    failure(output, "Conflicting entries for binary id alpha");
    for candidate in [&normal, &feature] {
        assert!(diagnostic.contains(candidate["binaries"][0]["executable"].as_str().unwrap()));
        assert!(diagnostic.contains(candidate["binaries"][0]["build-variant"].as_str().unwrap()));
    }
    let messages: Vec<Value> = std::fs::read_to_string(&w.messages)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let artifact = messages
        .iter()
        .find(|m| {
            m["profile"]["test"] == true
                && m["target"]["kind"] == json!(["lib"])
                && m["executable"].is_string()
        })
        .unwrap();
    let changed = w.dir.path().join("changed.jsonl");
    // Different feature metadata at the SAME executable path must still conflict.
    let mut variant = artifact.clone();
    variant["features"] = json!(["extra"]);
    std::fs::write(&changed, format!("{artifact}\n{variant}\n")).unwrap();
    failure(w.list(&changed).output().unwrap(), "Conflicting entries");
    variant = artifact.clone();
    variant["profile"]["opt_level"] = json!("3");
    std::fs::write(&changed, format!("{artifact}\n{variant}\n")).unwrap();
    failure(w.list(&changed).output().unwrap(), "Conflicting entries");
    variant = artifact.clone();
    variant["target_triple"] = json!("aarch64-unknown-linux-gnu");
    std::fs::write(&changed, format!("{artifact}\n{variant}\n")).unwrap();
    failure(w.list(&changed).output().unwrap(), "Conflicting entries");
    std::fs::write(&changed, format!("{artifact}\nnot JSON\n")).unwrap();
    failure(w.list(&changed).output().unwrap(), "changed.jsonl:2");
    variant = artifact.clone();
    variant["executable"] = json!(w.target.join("missing"));
    std::fs::write(&changed, variant.to_string()).unwrap();
    failure(w.list(&changed).output().unwrap(), "does not exist");
    failure(
        w.list(&w.dir.path().join("missing.jsonl"))
            .output()
            .unwrap(),
        "Failed to read Cargo messages",
    );
    std::fs::write(
        &changed,
        "{\"reason\":\"build-finished\",\"success\":true}\n",
    )
    .unwrap();
    failure(w.list(&changed).output().unwrap(), "No test binaries");

    // Nextest and Cargo inputs use exactly the same identity/conflict merge.
    let b = &normal["binaries"][0];
    let metadata = w.dir.path().join("nextest.json");
    let mut document = json!({"rust-build-meta": {"target-directory": w.target, "base-output-directories": ["debug"], "non-test-binaries": {}, "linked-paths": [], "target-platform": null}, "rust-binaries": {}});
    document["rust-binaries"]["alpha"] = json!({"binary-id": "alpha", "binary-name": b["name"], "kind": "lib", "package-id": b["package-id"], "binary-path": b["executable"], "build-platform": "target"});
    std::fs::write(&metadata, document.to_string()).unwrap();
    failure(
        w.list(&w.messages)
            .arg("--binaries-metadata")
            .arg(&metadata)
            .output()
            .unwrap(),
        "Conflicting entries for binary id alpha",
    );
    let beta: String = messages
        .iter()
        .filter(|m| {
            m["package_id"]
                .as_str()
                .is_some_and(|id| id.ends_with("#beta@0.1.0"))
        })
        .map(|m| format!("{m}\n"))
        .collect();
    std::fs::write(&changed, beta).unwrap();
    let mixed = success(
        w.list(&changed)
            .arg("--binaries-metadata")
            .arg(&metadata)
            .output()
            .unwrap(),
    );
    assert_eq!(
        mixed["binaries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["binary-id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "beta::shared"]
    );
    assert_eq!(mixed["inputs"].as_array().unwrap().len(), 2);
}
