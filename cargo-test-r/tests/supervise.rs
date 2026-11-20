use serde_json::json;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Suite {
    dir: tempfile::TempDir,
}

impl Suite {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            let source = dir.path().join("fixture.c");
            std::fs::write(&source, r#"
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
    if (!strcmp(mode, "slow")) { usleep(150000); return 0; }
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
"#).unwrap();
            assert!(
                Command::new("cc")
                    .arg(source)
                    .arg("-o")
                    .arg(dir.path().join("fixture"))
                    .status()
                    .unwrap()
                    .success()
            );
        }
        #[cfg(windows)]
        {
            let source = dir.path().join("fixture.rs");
            std::fs::write(&source, r#"
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
"#).unwrap();
            assert!(
                Command::new("rustc")
                    .arg(source)
                    .arg("-o")
                    .arg(dir.path().join("fixture.exe"))
                    .status()
                    .unwrap()
                    .success()
            );
        }
        for name in [
            "ok",
            "fail",
            "slow",
            "hung",
            "orphan",
            "escape",
            "resistant",
            "forward",
        ] {
            std::fs::copy(
                dir.path()
                    .join(format!("fixture{}", std::env::consts::EXE_SUFFIX)),
                dir.path()
                    .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
            )
            .unwrap();
        }
        Self { dir }
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    fn command(&self, names: &[&str]) -> Command {
        let binaries: Vec<_> = names.iter().enumerate().map(|(i, name)| json!({
            "binary-id": format!("pkg::{i}"), "package-id": "pkg@0.1.0", "package-name": "pkg",
            "kind": "test", "name": name, "build-variant": "variant",
            "executable": self.path().join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
            "working-directory": self.path()
        })).collect();
        std::fs::write(
            self.path().join("selection.json"),
            json!({
                "format": "cargo-test-r-selection", "version": 1, "inputs": [], "binaries": binaries
            })
            .to_string(),
        )
        .unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-test-r"));
        cmd.current_dir(self.path())
            .args(["run", "--selection", "selection.json"]);
        cmd
    }
    fn ready(&self) -> (u32, u16) {
        self.ready_file("ready")
    }
    fn ready_file(&self, file: &str) -> (u32, u16) {
        let start = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(self.path().join(file)) {
                let parts: Vec<_> = text.split_whitespace().collect();
                if parts.len() == 2 {
                    return (parts[0].parse().unwrap(), parts[1].parse().unwrap());
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "fixture never became ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn assert_port_closed(&self) {
        let (_, port) = self.ready();
        assert!(
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok(),
            "descendant retained port {port}"
        );
    }
}

// The receiving thread remains bounded even if a descendant retains a pipe.
fn finish(child: Child) -> std::process::Output {
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
    let output = finish(spawn(
        suite.command(&["fail", "slow", "hung", "ok"]).args([
            "--deadline",
            "300ms",
            "--no-fail-fast",
        ]),
    ));
    assert_eq!(output.status.code(), Some(124), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(suite.path().join("trace")).unwrap(),
        "fail\nslow\nhung\n"
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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-test-r"));
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
