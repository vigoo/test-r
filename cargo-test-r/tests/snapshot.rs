mod common;

use common::{failure, sha256_hex as hash, success, write_json as write};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command;

fn command(subcommand: &str, selection: &Path) -> Command {
    let mut command = common::cargo_test_r();
    command.arg(subcommand).arg("--selection").arg(selection);
    command
}

#[test]
fn verification_is_platform_independent_and_never_runs_the_loader() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    let executable = dir.path().join("bin/fixture.exe");
    // Invalid executable contents are intentional: verify must not execute them.
    fs::write(&executable, b"not an executable").unwrap();
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        0o755
    };
    #[cfg(not(unix))]
    let mode = 0;
    let selection = dir.path().join("selection.json");
    let mut doc = common::selection(&[executable], dir.path());
    // Before any snapshot field: a plain selection has nothing to verify.
    write(&selection, &doc);
    failure(
        command("verify", &selection).output().unwrap(),
        "Selection is not a snapshot",
    );
    // An option that cannot apply is an error, not silently ignored.
    for subcommand in ["run", "verify"] {
        failure(
            command(subcommand, &selection)
                .arg("--skip-library-hashes")
                .output()
                .unwrap(),
            "--skip-library-hashes applies only to snapshots",
        );
    }
    doc["snapshot"] = json!({"platform": "linux-gnu"});
    let binary = &mut doc["binaries"][0];
    binary["sha256"] = json!(hash(b"not an executable"));
    binary["mode"] = json!(mode);
    binary["dependencies"] = json!([]);
    write(&selection, &doc);
    success(command("verify", &selection).output().unwrap());
    failure(
        command("verify", &selection)
            .args(["--expect-digest", "sha256:wrong"])
            .output()
            .unwrap(),
        "digest mismatch",
    );
    doc["binaries"][0]["mode"] = json!(mode ^ 1);
    write(&selection, &doc);
    failure(
        command("verify", &selection).output().unwrap(),
        "mode mismatch",
    );
}

#[cfg(any(not(target_os = "linux"), target_env = "musl"))]
#[test]
fn snapshot_reports_unsupported_platform_without_creating_output() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("snapshot");
    failure(
        command("snapshot", &dir.path().join("unused.json"))
            .arg("--out")
            .arg(&out)
            .output()
            .unwrap(),
        "not supported",
    );
    assert!(!out.exists());
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
mod linux {
    use super::*;
    use serde_json::Value;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::PathBuf;
    use std::process::Output;
    use tempfile::TempDir;

    struct Fixture {
        dir: TempDir,
        target: PathBuf,
        metadata: PathBuf,
        selection: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target");
            fs::create_dir(&target).unwrap();
            Self {
                metadata: dir.path().join("metadata.json"),
                selection: dir.path().join("selection.json"),
                dir,
                target,
            }
        }

        fn compile(&self, name: &str, source: &str, flags: &[String]) -> PathBuf {
            let c = self.dir.path().join(format!("{name}.c"));
            fs::write(&c, source).unwrap();
            let binary = self.target.join(name);
            // Missing cc is a test failure, not a skip.
            success(
                Command::new("cc")
                    .arg(&c)
                    .arg("-o")
                    .arg(&binary)
                    .args(flags)
                    .output()
                    .expect("Linux snapshot tests require cc"),
            );
            binary
        }

        fn select(&self, binaries: &[(&str, &Path)], linked: &[PathBuf]) {
            let package = url::Url::from_directory_path(self.dir.path()).unwrap();
            let binaries: serde_json::Map<String, Value> = binaries.iter().map(|(id, path)| (id.to_string(), json!({
                "binary-id":id, "binary-name":id, "kind":"test", "package-id":format!("path+{package}#fixture@1.0.0"),
                "binary-path":path, "build-platform":"target"
            }))).collect();
            write(
                &self.metadata,
                &json!({"rust-build-meta":{
                "target-directory":self.target, "base-output-directories":["debug"], "non-test-binaries":{}, "linked-paths":linked
            }, "rust-binaries":binaries}),
            );
            let result = success(
                common::cargo_test_r()
                    .arg("list")
                    .arg("--binaries-metadata")
                    .arg(&self.metadata)
                    .output()
                    .unwrap(),
            );
            let doc: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert!(
                doc["binaries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|b| b.get("sha256").is_none()
                        && b.get("mode").is_none()
                        && b.get("dependencies").is_none())
            );
            fs::write(&self.selection, result.stdout).unwrap();
        }

        fn out(&self) -> PathBuf {
            self.dir.path().join("snapshot")
        }

        fn snapshot(&self) -> Output {
            command("snapshot", &self.selection)
                .arg("--out")
                .arg(self.out())
                .output()
                .unwrap()
        }

        fn manifest(&self) -> PathBuf {
            self.out().join("selection.json")
        }

        fn document(&self) -> Value {
            serde_json::from_slice(&fs::read(self.manifest()).unwrap()).unwrap()
        }

        fn assert_unpublished(&self) {
            assert!(!self.out().exists());
            assert!(fs::read_dir(self.dir.path()).unwrap().all(|e| {
                !e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".snapshot.tmp-")
            }));
        }
    }

    #[test]
    fn filtered_run_verifies_unselected_snapshot_bytes_and_keeps_the_pinned_manifest() {
        let f = Fixture::new();
        let first = f.compile(
            "first",
            "#include <stdio.h>\nint main(void) { puts(\"first\"); return 0; }",
            &[],
        );
        let second = f.compile("second", "int main(void) { return 7; }", &[]);
        f.select(&[("first", &first), ("second", &second)], &[]);
        success(f.snapshot());
        let original = fs::read(f.manifest()).unwrap();
        let digest = format!("sha256:{}", hash(&original));
        let run = || {
            command("run", &f.manifest())
                .args([
                    "--expect-digest",
                    &digest,
                    "--binary-id",
                    "first",
                    "--expect-count",
                    "1",
                ])
                .output()
                .unwrap()
        };
        assert_eq!(success(run()).stdout, b"first\n");
        assert_eq!(fs::read(f.manifest()).unwrap(), original);
        let document = f.document();
        let unselected = document["binaries"][1]["executable"].as_str().unwrap();
        fs::write(unselected, "tampered unselected executable").unwrap();
        let output = run();
        assert!(
            output.stdout.is_empty(),
            "no binary may start before full verification"
        );
        failure(output, "sha256 mismatch");
        fs::write(
            f.manifest(),
            format!("{} ", String::from_utf8(original).unwrap()),
        )
        .unwrap();
        failure(run(), "digest mismatch");
    }

    #[test]
    fn static_and_dynamic_independent_copies_survive_source_replacement() {
        let f = Fixture::new();
        let static_binary = f.compile(
            "static",
            "#include <stdio.h>\nint main(){puts(\"old static\");}",
            &["-static".into()],
        );
        let dynamic = f.compile(
            "dynamic",
            "#include <stdio.h>\nint main(){puts(\"old dynamic\");}",
            &[],
        );
        // Ids collide after sanitizing; full id hashes must disambiguate them.
        f.select(&[("a::same", &static_binary), ("a//same", &dynamic)], &[]);
        let original: Value = serde_json::from_slice(&fs::read(&f.selection).unwrap()).unwrap();
        let output = success(f.snapshot());
        let digest = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .last()
            .unwrap()
            .to_owned();
        assert_eq!(
            digest,
            format!("sha256:{}", hash(&fs::read(f.manifest()).unwrap()))
        );
        success(
            command("verify", &f.manifest())
                .args(["--expect-digest", &digest])
                .output()
                .unwrap(),
        );
        let doc = f.document();
        assert_eq!(doc["snapshot"], json!({"platform": "linux-gnu"}));
        assert_ne!(
            doc["binaries"][0]["executable"],
            doc["binaries"][1]["executable"]
        );
        for (index, entry) in doc["binaries"].as_array().unwrap().iter().enumerate() {
            let path = Path::new(entry["executable"].as_str().unwrap());
            let source = Path::new(original["binaries"][index]["executable"].as_str().unwrap());
            assert!(!path.symlink_metadata().unwrap().file_type().is_symlink());
            assert_ne!(
                fs::metadata(path).unwrap().ino(),
                fs::metadata(source).unwrap().ino()
            );
            assert_eq!(
                entry["working-directory"],
                original["binaries"][index]["working-directory"]
            );
            assert_eq!(entry["sha256"], hash(&fs::read(source).unwrap()));
            assert_eq!(
                entry["mode"],
                fs::metadata(source).unwrap().permissions().mode() & 0o7777
            );
        }
        assert!(doc["binaries"][0]["dependencies"].as_array().unwrap().len() >= 2);
        assert_eq!(doc["binaries"][1]["dependencies"], json!([]));
        f.compile("static", "int main(){return 19;}", &["-static".into()]);
        f.compile("dynamic", "int main(){return 23;}", &[]);
        fs::remove_file(&f.metadata).unwrap();
        let run = success(
            command("run", &f.manifest())
                .args(["--expect-digest", &digest])
                .output()
                .unwrap(),
        );
        assert_eq!(
            String::from_utf8(run.stdout).unwrap(),
            "old dynamic\nold static\n"
        );
        failure(f.snapshot(), "already exists");
    }

    #[test]
    fn tampering_is_rejected_before_any_binary_runs() {
        let f = Fixture::new();
        let exe = f.compile(
            "ok",
            "#include <stdio.h>\nint main(){puts(\"started\");}",
            &[],
        );
        f.select(&[("a", &exe), ("b", &exe)], &[]);
        let mut selection: Value =
            serde_json::from_slice(&fs::read(&f.selection).unwrap()).unwrap();
        assert_eq!(selection["inputs"][0]["kind"], "nextest-binaries");
        // Snapshot reads its inputs, so it must refuse kinds it does not know.
        selection["inputs"][0]["kind"] = json!("future-thing");
        write(&f.selection, &selection);
        failure(f.snapshot(), "written by a newer cargo-test-r");
        f.assert_unpublished();
        // Selections written before `kind` existed mean nextest metadata.
        selection["inputs"][0]
            .as_object_mut()
            .unwrap()
            .remove("kind");
        write(&f.selection, &selection);
        success(f.snapshot());
        let original = f.document();
        let digest = format!("sha256:{}", hash(&fs::read(f.manifest()).unwrap()));
        for mutation in ["hash", "mode", "outside", "missing", "contents", "partial"] {
            let mut doc = original.clone();
            let copied = PathBuf::from(doc["binaries"][1]["executable"].as_str().unwrap());
            let old_bytes = fs::read(&copied).unwrap();
            match mutation {
                "hash" => doc["binaries"][1]["sha256"] = json!("0".repeat(64)),
                "mode" => fs::set_permissions(&copied, fs::Permissions::from_mode(0o700)).unwrap(),
                "outside" => doc["binaries"][1]["executable"] = json!(exe),
                "missing" => {
                    fs::remove_file(&copied).unwrap();
                }
                "contents" => fs::write(&copied, b"changed").unwrap(),
                "partial" => {
                    doc["binaries"][1].as_object_mut().unwrap().remove("sha256");
                }
                _ => unreachable!(),
            }
            write(&f.manifest(), &doc);
            let run = command("run", &f.manifest()).output().unwrap();
            assert_eq!(run.status.code(), Some(1), "{mutation}: {run:?}");
            assert!(run.stdout.is_empty(), "A binary started for {mutation}");
            assert!(
                !command("verify", &f.manifest())
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            fs::write(&copied, old_bytes).unwrap();
            fs::set_permissions(&copied, fs::Permissions::from_mode(original["binaries"][1]["mode"].as_u64().unwrap() as u32)).unwrap();
        }
        // The marker makes a selection a snapshot; an older verifier must not
        // skip what it does not understand, nor treat a stripped marker as plain.
        let mut unmarked = original.clone();
        unmarked.as_object_mut().unwrap().remove("snapshot");
        let mut unknown_key = original.clone();
        unknown_key["snapshot"]["loader"] = json!("future");
        let mut unknown_platform = original.clone();
        unknown_platform["snapshot"]["platform"] = json!("future-os");
        for (doc, message) in [
            (unmarked, "snapshot fields but no \"snapshot\" marker"),
            (unknown_key, "unknown field `loader`"),
            (unknown_platform, "Unsupported snapshot platform"),
        ] {
            write(&f.manifest(), &doc);
            for subcommand in ["run", "verify"] {
                failure(
                    command(subcommand, &f.manifest()).output().unwrap(),
                    message,
                );
            }
        }
        // Without the marker and every per-binary field it is an ordinary selection.
        let mut plain = original.clone();
        plain.as_object_mut().unwrap().remove("snapshot");
        for binary in plain["binaries"].as_array_mut().unwrap() {
            for field in ["sha256", "mode", "dependencies"] {
                binary.as_object_mut().unwrap().remove(field);
            }
        }
        write(&f.manifest(), &plain);
        failure(
            command("verify", &f.manifest()).output().unwrap(),
            "Selection is not a snapshot",
        );
        let run = success(command("run", &f.manifest()).output().unwrap());
        assert_eq!(String::from_utf8(run.stdout).unwrap(), "started\nstarted\n");
        // An attacker updating both contents and the recorded hash still fails a pinned digest.
        let mut edited = original.clone();
        let copied = Path::new(edited["binaries"][1]["executable"].as_str().unwrap());
        fs::write(
            copied,
            fs::read(&exe)
                .unwrap()
                .into_iter()
                .chain([0])
                .collect::<Vec<_>>(),
        )
        .unwrap();
        edited["binaries"][1]["sha256"] = json!(hash(&fs::read(copied).unwrap()));
        write(&f.manifest(), &edited);
        failure(
            command("run", &f.manifest())
                .args(["--expect-digest", &digest])
                .output()
                .unwrap(),
            "digest mismatch",
        );
    }

    fn library_fixture(f: &Fixture, origin: bool, outside_target: bool) -> (PathBuf, PathBuf) {
        let library = f.compile(
            "libfixture.so",
            "int fixture(){return 0;}",
            &["-shared".into(), "-fPIC".into()],
        );
        let library = if outside_target {
            let outside = f.dir.path().join("libraries");
            fs::create_dir(&outside).unwrap();
            let dest = outside.join("libfixture.so");
            fs::rename(library, &dest).unwrap();
            dest
        } else {
            library
        };
        let root = library.parent().unwrap();
        let rpath = if origin {
            "$ORIGIN".to_owned()
        } else {
            root.to_str().unwrap().to_owned()
        };
        let exe = f.compile(
            "uses-library",
            "int fixture(); int main(){return fixture();}",
            &[
                format!("-L{}", root.display()),
                "-lfixture".into(),
                format!("-Wl,-rpath,{rpath}"),
            ],
        );
        (exe, library)
    }

    #[test]
    fn mutable_target_and_linked_paths_are_rejected() {
        for outside_target in [false, true] {
            let f = Fixture::new();
            let (exe, library) = library_fixture(&f, false, outside_target);
            let linked = if outside_target {
                vec![library.parent().unwrap().to_owned()]
            } else {
                vec![]
            };
            f.select(&[("mutable", &exe)], &linked);
            failure(f.snapshot(), "inside mutable target/linked-paths");
            f.assert_unpublished();
        }
    }

    #[test]
    fn origin_runpath_and_missing_libraries_are_rejected() {
        for remove_library in [false, true] {
            let f = Fixture::new();
            let (exe, library) = library_fixture(&f, !remove_library, false);
            f.select(&[("origin", &exe)], &[]);
            if remove_library {
                fs::remove_file(library).unwrap();
            }
            failure(f.snapshot(), "Loader dependency inspection failed");
            f.assert_unpublished();
        }
        // Both --list calls succeed, but $ORIGIN selects a different path before
        // relocation. Merely rejecting missing libraries would miss this case.
        let f = Fixture::new();
        let exe = f.compile(
            "origin-system",
            "#include <stdio.h>\nint main(){puts(\"ok\");}",
            &["-Wl,-rpath,$ORIGIN".into()],
        );
        f.select(&[("system", &exe)], &[]);
        success(f.snapshot());
        let doc = f.document();
        let libc = doc["binaries"][0]["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["path"].as_str().unwrap().ends_with("/libc.so.6"))
            .unwrap();
        std::os::unix::fs::symlink(libc["path"].as_str().unwrap(), f.target.join("libc.so.6"))
            .unwrap();
        fs::remove_dir_all(f.out()).unwrap();
        failure(f.snapshot(), "Library resolution changed");
        f.assert_unpublished();
    }

    #[test]
    fn third_input_failure_leaves_no_published_or_staging_directory() {
        let f = Fixture::new();
        let good = f.compile("good", "int main(){return 0;}", &[]);
        let bad = f.target.join("bad");
        fs::write(&bad, b"invalid ELF").unwrap();
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o755)).unwrap();
        f.select(&[("a", &good), ("b", &good), ("c", &bad)], &[]);
        failure(f.snapshot(), "requires an ELF");
        f.assert_unpublished();
    }

    #[test]
    fn recorded_libraries_are_hashed_unless_explicitly_skipped_but_must_exist() {
        let f = Fixture::new();
        let (exe, library) = library_fixture(&f, false, true);
        f.select(&[("admitted", &exe)], &[]);
        success(f.snapshot());
        let doc = f.document();
        assert!(
            doc["binaries"][0]["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["path"] == json!(library)
                    && d["sha256"] == hash(&fs::read(&library).unwrap()))
        );
        // Appending changes the hash while retaining a loadable ELF library.
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&library)
            .unwrap()
            .write_all(b"changed library")
            .unwrap();
        failure(
            command("verify", &f.manifest()).output().unwrap(),
            "Library sha256 mismatch",
        );
        failure(
            command("run", &f.manifest()).output().unwrap(),
            "Library sha256 mismatch",
        );
        success(
            command("verify", &f.manifest())
                .arg("--skip-library-hashes")
                .output()
                .unwrap(),
        );
        success(
            command("run", &f.manifest())
                .arg("--skip-library-hashes")
                .output()
                .unwrap(),
        );
        fs::remove_file(&library).unwrap();
        failure(
            command("verify", &f.manifest())
                .arg("--skip-library-hashes")
                .output()
                .unwrap(),
            "Missing recorded library",
        );
    }

    #[test]
    fn library_symlink_retarget_and_executable_symlink_escape_are_rejected() {
        let f = Fixture::new();
        let (exe, library) = library_fixture(&f, false, true);
        let actual = library.with_file_name("actual.so");
        fs::rename(&library, &actual).unwrap();
        std::os::unix::fs::symlink(&actual, &library).unwrap();
        f.select(&[("symlink", &exe)], &[]);
        success(f.snapshot());
        let other = library.with_file_name("other.so");
        fs::write(&other, b"wrong library").unwrap();
        fs::remove_file(&library).unwrap();
        std::os::unix::fs::symlink(&other, &library).unwrap();
        failure(
            command("verify", &f.manifest()).output().unwrap(),
            "Library sha256 mismatch",
        );
        fs::remove_file(&library).unwrap();
        std::os::unix::fs::symlink(&actual, &library).unwrap();
        let doc = f.document();
        let copied = Path::new(doc["binaries"][0]["executable"].as_str().unwrap());
        fs::remove_file(copied).unwrap();
        std::os::unix::fs::symlink(&exe, copied).unwrap();
        failure(
            command("run", &f.manifest()).output().unwrap(),
            "outside snapshot bin/",
        );
    }
}
