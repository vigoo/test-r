mod common;

use std::fs;
use std::process::Stdio;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[test]
fn zero_deadline_never_launches_preparation_or_creates_staging() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    for subcommand in ["list", "snapshot", "verify"] {
        let mut command = common::cargo_test_r();
        command.arg(subcommand).args(["--deadline", "0ms"]);
        command
            .arg(if subcommand == "list" {
                "--binaries-metadata"
            } else {
                "--selection"
            })
            .arg(&missing);
        if subcommand == "snapshot" {
            command.arg("--out").arg(dir.path().join("snapshot"));
        }
        let output = common::finish(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        assert_eq!(output.status.code(), Some(124), "{subcommand}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(
            output.stderr.is_empty(),
            "worker read missing input: {output:?}"
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[cfg(unix)]
#[test]
fn deadline_interrupts_blocked_input_reads_for_every_preparation_command() {
    use std::ffi::CString;
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("blocked.json");
    let name = CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for subcommand in ["list", "snapshot", "verify"] {
        if subcommand == "snapshot" && !cfg!(all(target_os = "linux", not(target_env = "musl"))) {
            continue;
        }
        let mut command = common::cargo_test_r();
        command.arg(subcommand).args(["--deadline", "1s"]);
        if subcommand != "list" {
            command.args(["--kill-grace", "20ms"]);
        }
        command
            .arg(if subcommand == "list" {
                "--binaries-metadata"
            } else {
                "--selection"
            })
            .arg(&fifo);
        if subcommand == "snapshot" {
            command.arg("--out").arg(dir.path().join("snapshot"));
        }
        let started = Instant::now();
        let output = common::finish(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        assert_eq!(output.status.code(), Some(124), "{subcommand}: {output:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
mod linux {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::process::{Child, Command};

    struct Fixture {
        dir: tempfile::TempDir,
        selection: PathBuf,
        marker: PathBuf,
        out: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let loader = dir.path().join("loader");
            fs::copy(
                common::cc_fixture(
                    r#"
#include <unistd.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(void) {
    const char *group = getenv("PREPARATION_EXPECT_GROUP");
    if (group) return getpgrp() == atoi(group) ? 0 : 7;
    signal(SIGTERM, SIG_IGN);
    int ready[2];
    if (pipe(ready)) return 3;
    pid_t pid = fork();
    if (pid < 0) return 3;
    if (pid == 0) {
        close(ready[0]);
        setsid();
        close(0); close(1); close(2);
        FILE *f = fopen(getenv("PREPARATION_MARKER"), "a");
        fprintf(f, "%d\n", getpid()); fclose(f);
        write(ready[1], "x", 1); close(ready[1]);
        for (;;) pause();
    }
    close(ready[1]);
    char byte;
    if (read(ready[0], &byte, 1) != 1) return 3;
    close(ready[0]);
    const char *mode = getenv("PREPARATION_MODE");
    if (!strcmp(mode, "block")) for (;;) pause();
    return !strcmp(mode, "fail") ? 7 : 0;
}
"#,
                ),
                &loader,
            )
            .unwrap();
            let target = dir.path().join("target");
            fs::create_dir(&target).unwrap();
            let source = dir.path().join("fixture.c");
            fs::write(&source, "void _start(void) { for (;;) {} }").unwrap();
            let executable = target.join("fixture");
            common::success(
                Command::new("cc")
                    .arg(&source)
                    .arg("-nostdlib")
                    .arg(format!("-Wl,--dynamic-linker={}", loader.display()))
                    .arg("-o")
                    .arg(&executable)
                    .output()
                    .unwrap(),
            );
            let metadata = dir.path().join("metadata.json");
            common::write_json(
                &metadata,
                &json!({"rust-build-meta": {
                "target-directory": target, "base-output-directories": ["debug"],
                "non-test-binaries": {}, "linked-paths": []
            }, "rust-binaries": {}}),
            );
            let mut doc = common::selection(&[executable], dir.path());
            doc["inputs"] = json!([{"path": metadata, "kind": "nextest-binaries",
                "sha256": common::sha256_hex(&fs::read(&metadata).unwrap())}]);
            let selection = dir.path().join("selection.json");
            common::write_json(&selection, &doc);
            Self {
                selection,
                marker: dir.path().join("pids"),
                out: dir.path().join("snapshot"),
                dir,
            }
        }

        fn spawn(&self, mode: &str, deadline: bool) -> Child {
            let mut command = common::cargo_test_r();
            command
                .arg("snapshot")
                .arg("--selection")
                .arg(&self.selection)
                .arg("--out")
                .arg(&self.out)
                .args(["--kill-grace", "20ms"])
                .env("PREPARATION_MODE", mode)
                .env("PREPARATION_MARKER", &self.marker)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if deadline {
                command.args(["--deadline", "1s"]);
            }
            command.spawn().unwrap()
        }

        fn assert_drained(&self) {
            let pids = fs::read_to_string(&self.marker).unwrap();
            assert!(!pids.is_empty());
            for pid in pids.lines().map(|pid| pid.parse::<i32>().unwrap()) {
                assert_eq!(
                    unsafe { libc::kill(pid, 0) },
                    -1,
                    "descendant {pid} survived"
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
            }
            assert!(
                fs::read_dir(self.dir.path()).unwrap().all(|entry| {
                    let name = entry.unwrap().file_name();
                    let name = name.to_string_lossy();
                    !name.starts_with(".cargo-test-r-preparation-")
                        && !name.starts_with(".snapshot.tmp-")
                }),
                "abandoned snapshot staging directory"
            );
        }
    }

    #[test]
    fn preparation_without_supervision_options_preserves_the_callers_process_group() {
        let fixture = Fixture::new();
        common::success(
            common::cargo_test_r()
                .arg("snapshot")
                .arg("--selection")
                .arg(&fixture.selection)
                .arg("--out")
                .arg(&fixture.out)
                .env(
                    "PREPARATION_EXPECT_GROUP",
                    unsafe { libc::getpgrp() }.to_string(),
                )
                .output()
                .unwrap(),
        );
        assert!(fixture.out.join("selection.json").exists());
    }

    #[test]
    fn deadline_kills_term_resistant_loader_and_escaped_descendants() {
        let fixture = Fixture::new();
        let output = common::finish(fixture.spawn("block", true));
        assert_eq!(output.status.code(), Some(124), "{output:?}");
        fixture.assert_drained();
        assert!(!fixture.out.exists());
    }

    #[test]
    fn deadline_interrupts_blocked_metadata_hashing() {
        use std::ffi::CString;
        let fixture = Fixture::new();
        let metadata = fixture.dir.path().join("metadata.json");
        fs::remove_file(&metadata).unwrap();
        let name = CString::new(metadata.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let output = common::finish(fixture.spawn("success", true));
        assert_eq!(output.status.code(), Some(124), "{output:?}");
        assert!(!fixture.marker.exists(), "loader must not have started");
        assert!(!fixture.out.exists());
        assert!(fs::read_dir(fixture.dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".cargo-test-r-preparation-")
        }));
    }

    #[test]
    fn cancelled_snapshot_drains_before_removing_partial_staging() {
        let fixture = Fixture::new();
        let child = fixture.spawn("block", false);
        common::wait_for_file(&fixture.marker, |text| text.ends_with('\n'));
        assert!(fs::read_dir(fixture.dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".cargo-test-r-preparation-")
        }));
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        let output = common::finish(child);
        assert_eq!(output.status.code(), Some(143), "{output:?}");
        fixture.assert_drained();
        assert!(!fixture.out.exists());
    }

    #[test]
    fn successful_and_failed_preparation_also_drain_loader_descendants() {
        for mode in ["success", "fail"] {
            let fixture = Fixture::new();
            let output = common::finish(fixture.spawn(mode, false));
            assert_eq!(
                output.status.success(),
                mode == "success",
                "{mode}: {output:?}"
            );
            fixture.assert_drained();
            assert_eq!(fixture.out.exists(), mode == "success");
            if mode == "success" {
                common::success(
                    common::cargo_test_r()
                        .arg("verify")
                        .arg("--selection")
                        .arg(fixture.out.join("selection.json"))
                        .output()
                        .unwrap(),
                );
            }
        }
    }
}
