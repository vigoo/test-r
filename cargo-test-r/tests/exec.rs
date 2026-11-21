mod common;

use common::{cargo_test_r, finish};
use std::io::Write;
use std::process::{Child, Command, Stdio};

fn command(program: &std::path::Path) -> Command {
    let mut cmd = cargo_test_r();
    cmd.args(["exec", "--kill-grace", "100ms", "--"])
        .arg(program);
    cmd
}

fn spawn(cmd: &mut Command) -> Child {
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn fixture() -> std::path::PathBuf {
    common::rustc_fixture(
        r#"
use std::io::{Read, Write};
fn main() {
    std::fs::write("launched", "yes").unwrap();
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "exit" => std::process::exit(args[1].parse().unwrap()),
        "inherit" => {
            assert_eq!(args[1..], ["", "a b", "--deadline", "literal;$HOME"]);
            assert_eq!(std::env::var("EXEC_TEST_VALUE").unwrap(), "inherited value");
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).unwrap();
            std::io::stdout().write_all(input.as_bytes()).unwrap();
            eprintln!("inherited stderr");
        }
        "hung" => loop { std::thread::sleep(std::time::Duration::from_secs(1)); },
        _ => panic!("unknown mode"),
    }
}
"#,
    )
}

#[test]
fn exit_codes_and_launch_failure() {
    let dir = tempfile::tempdir().unwrap();
    for code in [0, 1, 7, 124, 127, 143, 255] {
        let output = finish(spawn(
            command(&fixture())
                .current_dir(dir.path())
                .args(["exit", &code.to_string()]),
        ));
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    let output = finish(spawn(&mut command(&dir.path().join("missing"))));
    assert_eq!(output.status.code(), Some(127), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to launch command"));
}

#[test]
fn inherits_argv_environment_working_directory_and_standard_streams() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(
        command(&fixture())
            .current_dir(dir.path())
            .args(["inherit", "", "a b", "--deadline", "literal;$HOME"])
            .env("EXEC_TEST_VALUE", "inherited value")
            .stdin(Stdio::piped()),
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"stdin payload\n")
        .unwrap();
    let output = finish(child);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"stdin payload\n");
    assert_eq!(output.stderr, b"inherited stderr\n");
    assert!(dir.path().join("launched").exists());
}

#[test]
fn deadline_diagnostic_and_zero_budget_launches_nothing() {
    for deadline in ["0ms", "500ms"] {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = cargo_test_r();
        cmd.current_dir(dir.path())
            .args(["exec", "--deadline", deadline, "--kill-grace", "10ms", "--"])
            .arg(fixture())
            .arg("hung");
        let output = finish(spawn(&mut cmd));
        assert_eq!(output.status.code(), Some(124), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Command deadline expired"));
        assert_eq!(dir.path().join("launched").exists(), deadline != "0ms");
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn tree() -> std::path::PathBuf {
        common::cc_fixture(
            r#"
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <signal.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <netinet/in.h>
static void cleanup(int sig) {
    int fd = open("cleanup", O_CREAT|O_WRONLY, 0600);
    write(fd, "yes", 3); close(fd);
}
int main(int argc, char **argv) {
    if (!strcmp(argv[1], "crash")) { raise(SIGKILL); return 99; }
    int sync[2]; if (pipe(sync)) return 90;
    pid_t child = fork(); if (child < 0) return 91;
    if (!child) {
        close(sync[0]);
        if (!strcmp(argv[2], "escape") && setsid() < 0) _exit(92);
        signal(SIGTERM, cleanup); signal(SIGINT, cleanup); signal(SIGHUP, cleanup);
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in addr = {0}; addr.sin_family = AF_INET;
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if (bind(fd, (void*)&addr, sizeof(addr)) || listen(fd, 1)) _exit(93);
        socklen_t len = sizeof(addr); getsockname(fd, (void*)&addr, &len);
        FILE *ready = fopen("ready", "w");
        fprintf(ready, "%d %d %d", getppid(), getpid(), ntohs(addr.sin_port));
        fclose(ready);
        write(sync[1], "x", 1); close(sync[1]);
        while (1) pause();
    }
    close(sync[1]); char byte;
    if (read(sync[0], &byte, 1) != 1) return 94;
    close(sync[0]);
    if (!strcmp(argv[1], "hung")) while (1) pause();
    return atoi(argv[1]);
}
"#,
        )
    }

    fn ready(dir: &Path) -> (i32, i32, u16) {
        let text = common::wait_for_file(&dir.join("ready"), |s| s.split_whitespace().count() == 3);
        let fields: Vec<_> = text.split_whitespace().collect();
        (
            fields[0].parse().unwrap(),
            fields[1].parse().unwrap(),
            fields[2].parse().unwrap(),
        )
    }

    fn assert_gone(pid: i32, port: u16) {
        assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok());
        #[cfg(target_os = "linux")]
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "descendant {pid} was not reaped"
        );
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
    }

    fn tree_command(
        dir: &Path,
        mode: &str,
        escape: &str,
        grace: &str,
        deadline: Option<&str>,
    ) -> Command {
        let mut cmd = cargo_test_r();
        cmd.current_dir(dir).args(["exec", "--kill-grace", grace]);
        if let Some(deadline) = deadline {
            cmd.args(["--deadline", deadline]);
        }
        cmd.arg("--").arg(tree()).args([mode, escape]);
        cmd
    }

    #[test]
    fn preserves_command_signal() {
        let output = finish(spawn(command(&tree()).arg("crash")));
        assert_eq!(
            output.status.code(),
            Some(128 + libc::SIGKILL),
            "{output:?}"
        );
    }

    #[test]
    fn reaps_orphans_on_success_failure_and_direct_child_kill() {
        let escapes = if cfg!(target_os = "linux") {
            vec!["group", "escape"]
        } else {
            vec!["group"]
        };
        for escape in escapes {
            for mode in ["0", "7", "hung"] {
                let dir = tempfile::tempdir().unwrap();
                let child = spawn(&mut tree_command(dir.path(), mode, escape, "100ms", None));
                let (parent, descendant, port) = ready(dir.path());
                if mode == "hung" {
                    assert_eq!(unsafe { libc::kill(parent, libc::SIGKILL) }, 0);
                }
                let output = finish(child);
                let code = if mode == "hung" {
                    137
                } else {
                    mode.parse().unwrap()
                };
                assert_eq!(output.status.code(), Some(code), "{output:?}");
                assert_gone(descendant, port);
            }
        }
    }

    #[test]
    fn completed_command_cleanup_crosses_deadline_without_timeout() {
        for mode in ["0", "7"] {
            let dir = tempfile::tempdir().unwrap();
            let start = Instant::now();
            let child = spawn(&mut tree_command(
                dir.path(),
                mode,
                "group",
                "1200ms",
                Some("800ms"),
            ));
            let (_, descendant, port) = ready(dir.path());
            common::wait_for_file(&dir.path().join("cleanup"), |s| s == "yes");
            let output = finish(child);
            assert!(start.elapsed() >= Duration::from_millis(1200));
            assert_eq!(
                output.status.code(),
                Some(mode.parse().unwrap()),
                "{output:?}"
            );
            assert!(
                output.stderr.is_empty(),
                "cleanup was reported as timeout: {output:?}"
            );
            assert_gone(descendant, port);
        }
    }

    #[test]
    fn cancellation_during_work_and_completed_command_cleanup_wins() {
        for mode in ["hung", "0", "7"] {
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                let dir = tempfile::tempdir().unwrap();
                let child = spawn(&mut tree_command(dir.path(), mode, "group", "500ms", None));
                let (_, descendant, port) = ready(dir.path());
                if mode != "hung" {
                    common::wait_for_file(&dir.path().join("cleanup"), |s| s == "yes");
                }
                assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
                let output = finish(child);
                assert_eq!(output.status.code(), Some(128 + signal), "{output:?}");
                assert_gone(descendant, port);
            }
        }
    }

    #[test]
    fn cancellation_during_timeout_cleanup_overrides_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let child = spawn(&mut tree_command(
            dir.path(),
            "hung",
            "group",
            "2s",
            Some("500ms"),
        ));
        let (_, descendant, port) = ready(dir.path());
        common::wait_for_file(&dir.path().join("cleanup"), |s| s == "yes");
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        let output = finish(child);
        assert_eq!(output.status.code(), Some(130), "{output:?}");
        assert_gone(descendant, port);
    }

    #[test]
    fn cancellation_after_deadline_during_completed_command_cleanup_still_wins() {
        let dir = tempfile::tempdir().unwrap();
        let child = spawn(&mut tree_command(
            dir.path(),
            "7",
            "group",
            "2s",
            Some("500ms"),
        ));
        let (_, descendant, port) = ready(dir.path());
        common::wait_for_file(&dir.path().join("cleanup"), |s| s == "yes");
        // The command already exited. Cross its deadline while its resistant
        // descendant keeps cleanup active, then cancel the supervisor.
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        let output = finish(child);
        assert_eq!(output.status.code(), Some(143), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert_gone(descendant, port);
    }

    #[test]
    fn timeout_kills_resistant_descendants_but_not_unrelated_sibling() {
        let sibling_dir = tempfile::tempdir().unwrap();
        let mut sibling = Command::new(fixture())
            .current_dir(sibling_dir.path())
            .arg("hung")
            .spawn()
            .unwrap();
        common::wait_for_file(&sibling_dir.path().join("launched"), |s| s == "yes");
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let child = spawn(&mut tree_command(
            dir.path(),
            "hung",
            if cfg!(target_os = "linux") {
                "escape"
            } else {
                "group"
            },
            "100ms",
            Some("500ms"),
        ));
        let (_, descendant, port) = ready(dir.path());
        let output = finish(child);
        let sibling_alive = sibling.try_wait().unwrap().is_none();
        sibling.kill().unwrap();
        sibling.wait().unwrap();
        assert!(sibling_alive, "unrelated sibling was killed");
        assert_eq!(output.status.code(), Some(124), "{output:?}");
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_gone(descendant, port);
    }
}
