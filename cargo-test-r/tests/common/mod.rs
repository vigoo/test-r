//! Helpers shared by the cargo-test-r integration tests. Each test file uses a
//! subset, hence the module-wide `dead_code` allowance.
#![allow(dead_code)]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub fn cargo_test_r() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cargo-test-r"))
}

/// `name` with the platform's executable suffix.
pub fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A fixture executable compiled from Rust `source`. See [`compile`].
pub fn rustc_fixture(source: &str) -> PathBuf {
    compile("rustc", "rs", source)
}

/// A fixture executable compiled from C `source`. See [`compile`].
pub fn cc_fixture(source: &str) -> PathBuf {
    compile("cc", "c", source)
}

/// Bump when `compile` changes how fixtures are built.
const CACHE_VERSION: u32 = 1;

/// Compiles `source` at most once: the executable is cached under Cargo's
/// test temp directory, keyed by compiler version and source. Tests copy it (see
/// [`install`]) rather than running the cached file in place.
fn compile(compiler: &str, extension: &str, source: &str) -> PathBuf {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let version = Command::new(compiler)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("cannot run {compiler}: {e}"))
        .stdout;
    let key = sha256_hex(
        format!(
            "{CACHE_VERSION}\0{compiler}\0{}\0{source}",
            String::from_utf8_lossy(&version)
        )
        .as_bytes(),
    );
    let dir = root.join(format!("fixture-{}", &key[..16]));
    let executable = dir.join(exe("fixture"));
    if executable.exists() {
        return executable;
    }
    if dir.exists() {
        // A damaged entry would otherwise block publishing forever.
        let _ = std::fs::remove_dir_all(&dir);
    }
    // Compile into a private directory and publish it with one rename, so a
    // concurrent test process never sees a partial executable.
    let staging = tempfile::tempdir_in(root).unwrap();
    let file = staging.path().join(format!("fixture.{extension}"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(compiler)
        .arg(&file)
        .arg("-o")
        .arg(staging.path().join(exe("fixture")))
        .output()
        .unwrap_or_else(|e| panic!("cannot run {compiler}: {e}"));
    assert!(output.status.success(), "{compiler} failed: {output:?}");
    if std::fs::rename(staging.path(), &dir).is_err() {
        // Another test process published the same fixture first.
        assert!(executable.exists(), "cannot publish {}", dir.display());
    }
    executable
}

/// Copies `fixture` into `dir` once per name; fixtures select their behaviour
/// from their own file name.
pub fn install(fixture: &Path, dir: &Path, names: &[&str]) {
    for name in names {
        std::fs::copy(fixture, dir.join(exe(name))).unwrap();
    }
}

/// A version 1 selection running `executables` in order, all from `cwd`.
pub fn selection(executables: &[PathBuf], cwd: &Path) -> Value {
    let binaries: Vec<_> = executables
        .iter()
        .enumerate()
        .map(|(i, executable)| {
            json!({
                "binary-id": format!("pkg::{i}"), "package-id": "pkg@0.1.0", "package-name": "pkg",
                "kind": "test", "name": format!("test-{i}"), "build-variant": "variant",
                "executable": executable, "working-directory": cwd
            })
        })
        .collect();
    json!({"format": "cargo-test-r-selection", "version": 1, "inputs": [], "binaries": binaries})
}

pub fn write_json(path: &Path, document: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(document).unwrap()).unwrap();
}

pub fn success(output: Output) -> Output {
    assert!(output.status.success(), "{output:?}");
    output
}

/// Asserts success and parses stdout as JSON.
pub fn stdout_json(output: Output) -> Value {
    serde_json::from_slice(&success(output).stdout).unwrap()
}

/// Asserts a runner error: exit code 1, no stdout, and `message` on stderr.
pub fn failure(output: Output, message: &str) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
}

/// Polls `check` until it returns a value, failing after 10 seconds.
pub fn wait_until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits for `path` to have contents accepted by `complete`, and returns them.
pub fn wait_for_file(path: &Path, complete: impl Fn(&str) -> bool) -> String {
    wait_until(&format!("waiting for {}", path.display()), || {
        std::fs::read_to_string(path)
            .ok()
            .filter(|text| complete(text))
    })
}

/// Waits for `child` and collects its output. A watchdog kills it and fails
/// the test after 15 seconds, which also bounds a descendant holding a pipe.
pub fn finish(child: Child) -> Output {
    let pid = child.id();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    match receiver.recv_timeout(Duration::from_secs(15)) {
        Ok(output) => output.unwrap(),
        Err(error) => {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .status();
            }
            panic!("supervisor or inherited pipe exceeded watchdog: {error}");
        }
    }
}
