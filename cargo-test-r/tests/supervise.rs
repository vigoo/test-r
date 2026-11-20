mod common;

use common::{cargo_test_r, exe, finish, install};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

// Each copy selects its behaviour from its file name.
#[cfg(unix)]
fn fixture() -> PathBuf {
    common::cc_fixture(
        r#"
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <sys/socket.h>
#include <netinet/in.h>
static void received(int sig) { write(1, "forwarded\n", 10); }
int main(int argc, char **argv) {
    const char *mode = strrchr(argv[0], '/'); mode = mode ? mode+1 : argv[0];
    FILE *trace = fopen("trace", "a"); fprintf(trace, "%s\n", mode); fclose(trace);
    if (!strcmp(mode, "ok")) return 0;
    if (!strcmp(mode, "fail")) return 7;
    if (!strcmp(mode, "slow")) { usleep(1000000); return 0; }
    if (!strcmp(mode, "forward")) signal(SIGTERM, received);
    if (!strcmp(mode, "orphan") || !strcmp(mode, "escape") || !strcmp(mode, "resistant")) {
        signal(SIGTERM, SIG_IGN);
        signal(SIGINT, SIG_IGN);
        if (fork() != 0) { usleep(100000); return 0; }
        if (!strcmp(mode, "escape")) setsid();
        if (fork() != 0) return 0;
    }
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in addr = {0}; addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(fd, (void*)&addr, sizeof(addr)) || listen(fd, 1)) return 90;
    socklen_t len = sizeof(addr); getsockname(fd, (void*)&addr, &len);
    FILE *ready = fopen("ready", "w"); fprintf(ready, "%d %d", getpid(), ntohs(addr.sin_port)); fclose(ready);
    char name[80]; snprintf(name, sizeof(name), "ready-%s", mode);
    ready = fopen(name, "w"); fprintf(ready, "%d %d", getpid(), ntohs(addr.sin_port)); fclose(ready);
    while (1) pause();
}
"#,
    )
}

#[cfg(windows)]
fn fixture() -> PathBuf {
    common::rustc_fixture(
        r#"
fn main() {
    let exe = std::env::current_exe().unwrap();
    let mode = exe.file_stem().unwrap().to_str().unwrap();
    std::fs::write("trace", mode).unwrap();
    if mode == "ok" { return; }
    if mode == "orphan" {
        std::process::Command::new(exe.with_file_name("hung.exe")).spawn().unwrap();
        while !std::path::Path::new("ready").exists() { std::thread::sleep(std::time::Duration::from_millis(5)); }
        return;
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    std::fs::write("ready", format!("{} {}", std::process::id(), port.local_addr().unwrap().port())).unwrap();
    loop { std::thread::sleep(std::time::Duration::from_secs(1)); }
}
"#,
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
            &[
                "ok",
                "fail",
                "slow",
                "hung",
                "orphan",
                "escape",
                "resistant",
                "forward",
            ],
        );
        Self { dir }
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    fn command(&self, names: &[&str]) -> Command {
        let executables: Vec<_> = names
            .iter()
            .map(|name| self.path().join(exe(name)))
            .collect();
        common::write_json(
            &self.path().join("selection.json"),
            &common::selection(&executables, self.path()),
        );
        let mut cmd = cargo_test_r();
        cmd.current_dir(self.path())
            .args(["run", "--selection", "selection.json"]);
        cmd
    }
    fn ready(&self) -> (u32, u16) {
        self.ready_file("ready")
    }
    /// The pid and listening port a fixture publishes once it is running.
    fn ready_file(&self, file: &str) -> (u32, u16) {
        let text = common::wait_for_file(&self.path().join(file), |text| {
            text.split_whitespace().count() == 2
        });
        let (pid, port) = text.split_once(' ').unwrap();
        (pid.parse().unwrap(), port.parse().unwrap())
    }
    fn assert_port_closed(&self) {
        let (_, port) = self.ready();
        assert!(
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok(),
            "descendant retained port {port}"
        );
    }
}

fn spawn(cmd: &mut Command) -> Child {
    if !cmd.get_args().any(|arg| arg == "--kill-grace") {
        cmd.args(["--kill-grace", "100ms"]);
    }
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn normal_exit_and_duration_validation() {
    let suite = Suite::new();
    for duration in ["90s", "30m", "1h", "1000ms"] {
        assert_eq!(
            finish(spawn(suite.command(&["ok"]).args(["--deadline", duration])))
                .status
                .code(),
            Some(0)
        );
    }
    for duration in ["", "3", "-1s", "1.5m", "1d", "18446744073709551615h"] {
        assert_eq!(
            suite
                .command(&["ok"])
                .args(["--deadline", duration])
                .output()
                .unwrap()
                .status
                .code(),
            Some(2)
        );
    }
    std::fs::remove_file(suite.path().join("trace")).unwrap();
    assert_eq!(
        finish(spawn(suite.command(&["ok"]).args(["--deadline", "0s"])))
            .status
            .code(),
        Some(124)
    );
    assert!(!suite.path().join("trace").exists());
}

#[test]
fn hung_binary_deadline_closes_pipe_and_port() {
    let suite = Suite::new();
    let child = spawn(suite.command(&["hung", "ok"]).args([
        "--deadline",
        "500ms",
        "--no-fail-fast",
        "--keep-leftovers",
    ]));
    suite.ready();
    let output = finish(child);
    assert_eq!(output.status.code(), Some(124), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Invocation deadline expired"),
        "{output:?}"
    );
    suite.assert_port_closed();
    assert!(
        !std::fs::read_to_string(suite.path().join("trace"))
            .unwrap()
            .contains("ok")
    );
}

#[test]
fn orphan_grandchild_loses_inherited_pipe_and_listening_port() {
    let suite = Suite::new();
    let output = finish(spawn(&mut suite.command(&["orphan", "ok"])));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    suite.assert_port_closed();
    #[cfg(target_os = "linux")]
    assert_eq!(
        unsafe { libc::kill(suite.ready().0 as i32, 0) },
        -1,
        "orphan was not reaped"
    );
}

#[cfg(unix)]
#[test]
fn term_resistant_child_escalates_and_shared_deadline_stops_next_binary() {
    let suite = Suite::new();
    let start = Instant::now();
    let output = finish(spawn(suite.command(&["resistant", "ok"]).args([
        "--deadline",
        "150ms",
        "--no-fail-fast",
    ])));
    assert_eq!(output.status.code(), Some(124), "{output:?}");
    assert!(start.elapsed() < Duration::from_secs(5));
    suite.assert_port_closed();
    assert_eq!(
        std::fs::read_to_string(suite.path().join("trace")).unwrap(),
        "resistant\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn subreaper_cleans_descendant_that_escaped_group() {
    let suite = Suite::new();
    assert_eq!(
        finish(spawn(&mut suite.command(&["escape"]))).status.code(),
        Some(0)
    );
    suite.assert_port_closed();
    assert_eq!(unsafe { libc::kill(suite.ready().0 as i32, 0) }, -1);
}

#[cfg(unix)]
#[test]
fn keep_leftovers_preserves_daemon_after_normal_exit() {
    let suite = Suite::new();
    let mut child = suite
        .command(&["orphan"])
        .arg("--keep-leftovers")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let (pid, port) = suite.ready();
    assert!(child.wait().unwrap().success());
    assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(unix)]
#[test]
fn signals_override_failure_and_second_interrupt_skips_grace() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let suite = Suite::new();
        let child = spawn(suite.command(&["fail", "resistant", "ok"]).args([
            "--no-fail-fast",
            "--kill-grace",
            "10s",
        ]));
        suite.ready();
        let start = Instant::now();
        unsafe {
            libc::kill(child.id() as i32, signal);
        }
        std::thread::sleep(Duration::from_millis(50));
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        let output = finish(child);
        assert_eq!(output.status.code(), Some(128 + signal), "{output:?}");
        assert!(start.elapsed() < Duration::from_secs(5));
        suite.assert_port_closed();
    }
}

#[cfg(unix)]
#[test]
fn received_signal_is_forwarded_once_not_on_every_poll() {
    let suite = Suite::new();
    let child = spawn(suite.command(&["forward"]).args(["--kill-grace", "200ms"]));
    suite.ready();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let output = finish(child);
    assert_eq!(output.status.code(), Some(143));
    assert_eq!(output.stdout, b"forwarded\n");
    suite.assert_port_closed();
}

#[cfg(unix)]
#[test]
fn keep_leftovers_does_not_protect_an_earlier_daemon_from_cancellation() {
    let suite = Suite::new();
    let child = spawn(suite.command(&["orphan", "hung"]).arg("--keep-leftovers"));
    let (_, daemon_port) = suite.ready_file("ready-orphan");
    let (_, active_port) = suite.ready_file("ready-hung");
    assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, daemon_port)).is_err());
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    assert_eq!(finish(child).status.code(), Some(130));
    for port in [daemon_port, active_port] {
        assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok());
    }
}

#[cfg(unix)]
#[test]
fn cancellation_at_binary_handoff_never_leaks_spawned_child() {
    let suite = Suite::new();
    for delay in [0, 1, 5, 15, 25, 50] {
        let _ = std::fs::remove_file(suite.path().join("trace"));
        let _ = std::fs::remove_file(suite.path().join("ready"));
        let child = spawn(suite.command(&["ok", "hung"]).args(["--no-fail-fast"]));
        // First binary publishing trace proves the runner installed handlers; the
        // varying delay straddles admission/spawn of the second real process.
        let start = Instant::now();
        while !suite.path().join("trace").exists() {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(delay));
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        assert_eq!(finish(child).status.code(), Some(130));
        if suite.path().join("ready").exists() {
            suite.assert_port_closed();
        }
    }
}

#[cfg(unix)]
#[test]
fn one_budget_spans_binaries_and_timeout_overrides_earlier_failure() {
    let suite = Suite::new();
    // `slow` takes 1s and the budget is 1.5s, leaving `hung` a wide margin to
    // start even on slow CI runners. A per-binary budget would instead let
    // `hung` run for a full 1.5s after it starts.
    let output = finish(spawn(suite.command(&["fail", "slow", "hung", "ok"]).args(
        [
            "--deadline",
            "1500ms",
            "--no-fail-fast",
            "--events",
            "events.jsonl",
        ],
    )));
    assert_eq!(output.status.code(), Some(124), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(suite.path().join("trace")).unwrap(),
        "fail\nslow\nhung\n"
    );
    // Both times come from the runner's monotonic invocation clock, so a slow
    // machine delays them together instead of failing a wall-clock bound.
    let records: Vec<serde_json::Value> =
        std::fs::read_to_string(suite.path().join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let elapsed = |event: &str, binary: &str| {
        let record = records
            .iter()
            .find(|r| r["event"] == event && r["binary-id"] == binary)
            .unwrap_or_else(|| panic!("no {event} for {binary}: {records:?}"));
        record["elapsed-ms"].as_u64().unwrap()
    };
    let hung_started = elapsed("binary-started", "pkg::2");
    let expired = elapsed("deadline-expired", "pkg::2");
    assert!(expired >= 1500, "expired early at {expired}ms");
    assert!(
        expired < hung_started + 1500,
        "budget restarted for hung (started {hung_started}ms, expired {expired}ms)"
    );
}

#[cfg(unix)]
#[test]
fn cargo_fallback_deadline_includes_build_and_strips_only_runner_flags() {
    use std::os::unix::fs::PermissionsExt;
    let suite = Suite::new();
    std::fs::write(
        suite.path().join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n[workspace]\n",
    )
    .unwrap();
    std::fs::create_dir(suite.path().join("src")).unwrap();
    std::fs::write(suite.path().join("src/lib.rs"), "").unwrap();
    std::fs::write(suite.path().join("Cargo.lock"), "").unwrap();
    let cargo = suite.path().join("cargo");
    std::fs::write(
        &cargo,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > args\nexec ./hung\n",
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = cargo_test_r();
    cmd.current_dir(suite.path()).env("CARGO", cargo).args([
        "run",
        "--deadline=500ms",
        "--kill-grace",
        "10ms",
        "--keep-leftovers",
        "--no-fail-fast",
        "--",
        "--deadline",
        "harness-value",
    ]);
    assert_eq!(finish(spawn(&mut cmd)).status.code(), Some(124));
    assert_eq!(
        std::fs::read_to_string(suite.path().join("args")).unwrap(),
        "test\n--no-fail-fast\n--\n--deadline\nharness-value\n"
    );
    suite.assert_port_closed();
}

/// Signal handlers are installed only once processes are about to run, so an
/// interrupt while the selection is still being read takes the default action.
#[cfg(unix)]
#[test]
fn interrupt_while_reading_selection_terminates_by_default_action() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::ExitStatusExt;
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("selection.json");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut cmd = cargo_test_r();
    cmd.current_dir(dir.path())
        .args(["run", "--selection", "selection.json"]);
    // The test runner may inherit an ignored SIGINT (a background job, for
    // example). Restore the disposition a terminal-started runner has.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            Ok(())
        });
    }
    let child = spawn(&mut cmd);
    // A non-blocking writer can open a FIFO only once a reader has it open:
    // success proves the runner is reading the selection. Holding the writer
    // open without writing keeps that read blocked.
    let _writer = common::wait_until("runner to open the selection FIFO", || {
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .ok()
    });
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let status = finish(child).status;
    // Death by the default action, not a handled cancellation (exit 130).
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
}

/// Without a deadline, the Cargo fallback is a plain pass-through: Cargo stays
/// in the runner's process group (the terminal's foreground group). With one,
/// it is supervised in a group of its own.
#[cfg(unix)]
#[test]
fn cargo_fallback_runs_in_runner_group_unless_supervised() {
    use std::os::unix::process::CommandExt;
    let cargo = common::cc_fixture(
        r#"
#include <stdio.h>
#include <unistd.h>
int main(void) {
    FILE *file = fopen("pgid", "w"); fprintf(file, "%d", (int)getpgrp()); fclose(file);
    return 0;
}
"#,
    );
    for deadline in [false, true] {
        // A Cargo.lock without target/nextest-archive selects the fallback.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "").unwrap();
        common::install(&cargo, dir.path(), &["cargo"]);
        let mut cmd = cargo_test_r();
        cmd.current_dir(dir.path())
            .env("CARGO", dir.path().join(common::exe("cargo")))
            .arg("run")
            .process_group(0);
        if deadline {
            cmd.args(["--deadline", "30s"]);
        }
        let child = spawn(&mut cmd);
        let runner_group = child.id().to_string();
        let output = finish(child);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let cargo_group = std::fs::read_to_string(dir.path().join("pgid")).unwrap();
        if deadline {
            assert_ne!(cargo_group, runner_group);
        } else {
            assert_eq!(cargo_group, runner_group);
        }
    }
}
