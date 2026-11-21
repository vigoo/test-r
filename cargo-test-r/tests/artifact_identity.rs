mod common;

use common::{cargo_test_r, failure, stdout_json};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    dir: tempfile::TempDir,
    metadata: PathBuf,
    messages: PathBuf,
    artifact: Value,
    authority: Value,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        #[cfg(unix)]
        let root = dir.path().canonicalize().unwrap();
        #[cfg(not(unix))]
        let root = std::path::absolute(dir.path()).unwrap();
        let package = root.join("member");
        std::fs::create_dir_all(package.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=['member']\n").unwrap();
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname='sample'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(package.join("src/lib.rs"), "").unwrap();
        std::fs::write(package.join("src/decoy.rs"), "").unwrap();
        std::fs::write(package.join("decoy.toml"), "").unwrap();
        common::install(&common::rustc_fixture("fn main() {}"), &root, &["test"]);
        let id = format!(
            "path+{}#sample@0.1.0",
            url::Url::from_directory_path(&package)
                .unwrap()
                .as_str()
                .trim_end_matches('/')
        );
        let target =
            json!({"kind": ["lib"], "name": "sample", "src_path": package.join("src/lib.rs")});
        let authority = json!({
            "version": 1, "workspace_root": root, "workspace_members": [id],
            "packages": [{"id": id, "name": "sample", "manifest_path": package.join("Cargo.toml"), "targets": [target]}]
        });
        let artifact = json!({
            "reason": "compiler-artifact", "package_id": id,
            "manifest_path": package.join("Cargo.toml"), "target": target,
            "profile": {"test": true, "opt_level": "0"}, "features": ["a", "b"],
            "executable": root.join(common::exe("test"))
        });
        let fixture = Self {
            metadata: root.join("metadata.json"),
            messages: root.join("messages.jsonl"),
            dir,
            artifact,
            authority,
        };
        fixture.write_authority(&fixture.authority);
        fixture.write_messages(std::slice::from_ref(&fixture.artifact));
        fixture
    }

    fn write_authority(&self, value: &Value) {
        common::write_json(&self.metadata, value);
    }

    fn write_messages(&self, values: &[Value]) {
        let lines: String = values.iter().map(|value| format!("{value}\n")).collect();
        std::fs::write(&self.messages, lines).unwrap();
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut command = cargo_test_r();
        command
            .current_dir(self.dir.path())
            .env("CARGO", "must-not-invoke-cargo")
            .arg(subcommand)
            .arg("--cargo-messages")
            .arg(&self.messages)
            .arg("--cargo-metadata")
            .arg(&self.metadata);
        command
    }

    fn rejects(&self, expected: &str) {
        let output = self.command("list").output().unwrap();
        let diagnostic = String::from_utf8_lossy(&output.stderr).into_owned();
        failure(output, expected);
        assert!(diagnostic.contains("messages.jsonl:"), "{diagnostic}");
        assert!(diagnostic.contains("metadata.json"), "{diagnostic}");
    }
}

#[test]
fn metadata_identity_accepts_equivalent_duplicates_and_records_provenance() {
    let f = Fixture::new();
    let selection = stdout_json(f.command("list").output().unwrap());
    assert_eq!(selection["binaries"].as_array().unwrap().len(), 1);
    let input = selection["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|input| input["kind"] == "cargo-metadata")
        .unwrap();
    assert_eq!(Path::new(input["path"].as_str().unwrap()), f.metadata);
    assert_eq!(
        input["sha256"],
        common::sha256_hex(&std::fs::read(&f.metadata).unwrap())
    );
    let mut equivalent = f.artifact.clone();
    equivalent["features"] = json!(["b", "a", "b"]);
    f.write_messages(&[f.artifact.clone(), equivalent]);
    let duplicate = stdout_json(f.command("list").output().unwrap());
    assert_eq!(selection["binaries"], duplicate["binaries"]);
    common::success(f.command("run").output().unwrap());
}

#[test]
fn metadata_uses_canonical_paths_and_preserves_multi_kind_target_identity() {
    let f = Fixture::new();
    let mut authority = f.authority.clone();
    authority["packages"][0]["manifest_path"] =
        json!(f.dir.path().join("member/src/../Cargo.toml"));
    authority["packages"][0]["targets"][0]["kind"] = json!(["rlib", "dylib"]);
    f.write_authority(&authority);
    let mut artifact = f.artifact.clone();
    artifact["target"]["src_path"] = json!(f.dir.path().join("member/src/../src/lib.rs"));
    artifact["target"]["kind"] = json!(["rlib", "dylib"]);
    f.write_messages(std::slice::from_ref(&artifact));
    let selection = stdout_json(f.command("list").output().unwrap());
    assert_eq!(selection["binaries"][0]["kind"], "lib");
    artifact["target"]["kind"] = json!(["dylib", "rlib"]);
    f.write_messages(&[artifact]);
    f.rejects("target name/kind/src_path");
}

#[test]
fn raw_wrong_sources_and_original_kind_vectors_cannot_hide_in_duplicates() {
    let f = Fixture::new();
    for (pointer, value, diagnostic) in [
        (
            "/target/src_path",
            json!(
                f.dir
                    .path()
                    .join("member/src/decoy.rs")
                    .canonicalize()
                    .unwrap()
            ),
            "target name/kind/src_path",
        ),
        (
            "/manifest_path",
            json!(
                f.dir
                    .path()
                    .join("member/decoy.toml")
                    .canonicalize()
                    .unwrap()
            ),
            "Artifact manifest",
        ),
        ("/target/kind", json!(["rlib"]), "target name/kind/src_path"),
        (
            "/target/kind",
            json!(["lib", "rlib"]),
            "target name/kind/src_path",
        ),
        ("/target/name", json!("decoy"), "target name/kind/src_path"),
        (
            "/target/src_path",
            Value::Null,
            "Missing artifact target src_path",
        ),
        (
            "/target/src_path",
            json!(123),
            "Missing artifact target src_path",
        ),
    ] {
        let mut decoy = f.artifact.clone();
        *decoy.pointer_mut(pointer).unwrap() = value;
        // The invalid entry must fail on either side of the valid entry.
        for values in [
            vec![f.artifact.clone(), decoy.clone()],
            vec![decoy.clone(), f.artifact.clone()],
        ] {
            f.write_messages(&values);
            f.rejects(diagnostic);
        }
    }
    // No authority keeps the original source-agnostic selection contract.
    let output = cargo_test_r()
        .arg("list")
        .arg("--cargo-messages")
        .arg(&f.messages)
        .output()
        .unwrap();
    assert_eq!(stdout_json(output)["binaries"].as_array().unwrap().len(), 1);
}

#[test]
fn package_and_workspace_authority_are_checked_independently() {
    let f = Fixture::new();
    let mut decoy = f.artifact.clone();
    decoy["package_id"] = json!(format!(
        "path+{}#sample@0.1.0",
        url::Url::from_directory_path(f.dir.path().canonicalize().unwrap())
            .unwrap()
            .as_str()
            .trim_end_matches('/')
    ));
    f.write_messages(std::slice::from_ref(&decoy));
    f.rejects("Expected exactly one Cargo metadata package");

    // Even matching metadata IDs cannot claim a different package source directory.
    let mut authority = f.authority.clone();
    authority["packages"][0]["id"] = decoy["package_id"].clone();
    authority["workspace_members"][0] = decoy["package_id"].clone();
    f.write_authority(&authority);
    f.rejects("Package source directory");

    f.write_messages(std::slice::from_ref(&f.artifact));
    for (pointer, value, diagnostic) in [
        ("/packages/0/name", json!("other"), "Package name"),
        ("/workspace_members", json!([]), "not a member"),
        (
            "/packages/0/targets/0/src_path",
            json!(
                f.dir
                    .path()
                    .join("member/src/decoy.rs")
                    .canonicalize()
                    .unwrap()
            ),
            "target name/kind/src_path",
        ),
    ] {
        let mut authority = f.authority.clone();
        *authority.pointer_mut(pointer).unwrap() = value;
        f.write_authority(&authority);
        f.rejects(diagnostic);
    }
}

#[test]
fn cargo_metadata_accepts_members_outside_the_workspace_root() {
    let f = Fixture::new();
    std::fs::remove_file(f.dir.path().join("Cargo.toml")).unwrap();
    let workspace = f.dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nmembers=['../member']\nresolver='3'\n",
    )
    .unwrap();
    std::fs::write(
        f.dir.path().join("member/Cargo.toml"),
        "[package]\nname='sample'\nversion='0.1.0'\nworkspace='../workspace'\n",
    )
    .unwrap();
    let authority = stdout_json(
        Command::new(env!("CARGO"))
            .current_dir(&workspace)
            .args([
                "metadata",
                "--no-deps",
                "--offline",
                "--format-version",
                "1",
            ])
            .output()
            .unwrap(),
    );
    f.write_authority(&authority);
    let selection = stdout_json(f.command("list").output().unwrap());
    assert_eq!(selection["binaries"].as_array().unwrap().len(), 1);
    assert_eq!(
        selection["binaries"][0]["package-id"],
        f.artifact["package_id"]
    );
    common::success(f.command("run").output().unwrap());
}

#[cfg(unix)]
#[test]
fn symlinked_manifests_preserve_package_directory_identity() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    std::fs::rename(
        f.dir.path().join("member/Cargo.toml"),
        f.dir.path().join("package.toml"),
    )
    .unwrap();
    symlink("../package.toml", f.dir.path().join("member/Cargo.toml")).unwrap();
    let authority = stdout_json(
        Command::new(env!("CARGO"))
            .current_dir(f.dir.path())
            .args([
                "metadata",
                "--no-deps",
                "--offline",
                "--format-version",
                "1",
            ])
            .output()
            .unwrap(),
    );
    f.write_authority(&authority);
    let selected = stdout_json(f.command("list").output().unwrap());
    assert_eq!(selected["binaries"].as_array().unwrap().len(), 1);
    common::success(f.command("run").output().unwrap());

    let decoy = f.dir.path().join("decoy");
    std::fs::create_dir(&decoy).unwrap();
    symlink("../member/Cargo.toml", decoy.join("Cargo.toml")).unwrap();
    let mut alias = f.artifact.clone();
    alias["manifest_path"] = json!(decoy.join("Cargo.toml"));
    for artifacts in [
        vec![f.artifact.clone(), alias.clone()],
        vec![alias, f.artifact.clone()],
    ] {
        f.write_messages(&artifacts);
        f.rejects("Artifact manifest directory");
    }
}

#[test]
fn authority_does_not_weaken_variant_conflicts_or_validate_excluded_artifacts() {
    let f = Fixture::new();
    for (pointer, value) in [
        ("/features", json!(["extra"])),
        ("/profile/opt_level", json!("3")),
    ] {
        let mut variant = f.artifact.clone();
        *variant.pointer_mut(pointer).unwrap() = value;
        f.write_messages(&[f.artifact.clone(), variant]);
        failure(
            f.command("list").output().unwrap(),
            "Conflicting entries for binary id sample",
        );
    }
    let mut variant = f.artifact.clone();
    variant["target_triple"] = json!("aarch64-unknown-linux-gnu");
    f.write_messages(&[f.artifact.clone(), variant]);
    failure(
        f.command("list").output().unwrap(),
        "Conflicting entries for binary id sample",
    );
    let mut unselected = f.artifact.clone();
    unselected["package_id"] = json!("path+file:///not-the-workspace#excluded@0.1.0");
    unselected["target"]["src_path"] = Value::Null;
    f.write_messages(&[f.artifact.clone(), unselected]);
    let selected = stdout_json(
        f.command("list")
            .args(["--package", "sample"])
            .output()
            .unwrap(),
    );
    assert_eq!(selected["binaries"].as_array().unwrap().len(), 1);
}

#[test]
fn metadata_assertion_cannot_be_silently_ignored_without_messages() {
    let f = Fixture::new();
    let output = cargo_test_r()
        .arg("list")
        .arg("--cargo-metadata")
        .arg(&f.metadata)
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let nextest = f.dir.path().join("nextest.json");
    std::fs::write(&nextest, "{}").unwrap();
    let output = f
        .command("list")
        .arg("--binaries-metadata")
        .arg(nextest)
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}
