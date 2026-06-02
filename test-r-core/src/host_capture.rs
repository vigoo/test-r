//! Parent-side capture of stdout/stderr written outside the test bodies
//! (e.g. by `HostedRpc` owner constructors, owner dispatch methods, and
//! any background threads/tasks/subprocesses they spawn).
//!
//! Workers stream their own stdout/stderr through dedicated pipes the
//! parent already drains line-by-line and partitions per test (see
//! `Worker::drain_until`). The parent process itself has no such
//! plumbing for the host-side prints — they go directly to the
//! inherited stdout/stderr and either land on the user's terminal (when
//! the parent's stdio is connected to one) or get swallowed by an outer
//! wrapper (`cargo test` without `-- --nocapture`, structured CI loggers,
//! etc.). They are also missing from every structured output format
//! (`--format=json`/`junit`/`ctrf`) and can fragment those streams when
//! they sneak through.
//!
//! This module installs an opt-in capture in the top-level parent
//! whenever the runner is going to spawn worker subprocesses
//! (i.e. capture is on). It:
//!
//! 1. `dup(1)` / `dup(2)` into a pair of owned terminal fds kept aside
//!    so the formatter can keep writing to the *real* terminal;
//! 2. creates a single pipe and `dup2(write_end, 1)` / `dup2(write_end, 2)`
//!    so every later stdout/stderr write from anything running in the
//!    parent (including host-side dep owners) ends up in the pipe;
//! 3. spawns a reader thread that line-buffers the pipe and spills each
//!    line to a binary record file under `std::env::temp_dir()`.
//!
//! The temp file is the host log. It is bounded only by disk space.
//! Step 1 of the feature lands the plumbing without surfacing the log
//! anywhere — subsequent steps will read it back at suite end and feed
//! it through the formatters.
//!
//! The capture is a no-op in IPC worker subprocesses and in
//! `--nocapture` mode. It is supported on Unix (via `dup`/`dup2`) and
//! on Windows (via `GetStdHandle`/`SetStdHandle` against an anonymous
//! `CreatePipe`); on other targets it falls back to a no-op stub.

#![allow(dead_code)]

use std::io::{self, IsTerminal, Write};
#[cfg(any(unix, windows))]
use std::path::{Path, PathBuf};
#[cfg(any(unix, windows))]
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

use crate::args::Arguments;

/// A single record parsed from the host-capture spill file.
///
/// `elapsed` is the offset from [`HostCapture::epoch`] at which the
/// line landed in the parent's redirected stdout/stderr pipe.
/// `stream_tag` is currently always `0` ("mixed") because both fd 1
/// and fd 2 are redirected to the same pipe; the field is kept so a
/// later step can split the pipe and tag stdout vs stderr without an
/// on-disk format break.
/// `line` is the line bytes with the trailing `\n` (and any preceding
/// `\r`) already stripped, decoded with [`String::from_utf8_lossy`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct HostLogRecord {
    pub elapsed: Duration,
    pub stream_tag: u8,
    pub line: String,
}

/// Cached "is the real stdout a terminal" answer captured before any
/// redirection installs. Read by `terminal_stdout_is_terminal()` and
/// queried by the formatters (`term_progress`, `progress`) to decide
/// whether to emit ANSI / OSC sequences. When unset, callers fall back
/// to `io::stdout().is_terminal()`.
static REAL_STDOUT_IS_TERMINAL: OnceLock<bool> = OnceLock::new();
static REAL_STDERR_IS_TERMINAL: OnceLock<bool> = OnceLock::new();

/// Returns `true` if the parent process's original stdout was connected
/// to a terminal. Cached by `install_if_needed`; falls back to a live
/// check when host capture is not active.
pub(crate) fn terminal_stdout_is_terminal() -> bool {
    *REAL_STDOUT_IS_TERMINAL.get_or_init(|| io::stdout().is_terminal())
}

/// Returns `true` if the parent process's original stderr was connected
/// to a terminal. Cached by `install_if_needed`; falls back to a live
/// check when host capture is not active.
pub(crate) fn terminal_stderr_is_terminal() -> bool {
    *REAL_STDERR_IS_TERMINAL.get_or_init(|| io::stderr().is_terminal())
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::fs::File;
    use std::io::{BufRead, BufReader};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Instant;
    use uuid::Uuid;

    /// Active capture handle. Drop restores fd 1/2 from the saved
    /// terminal fds, closes the held write end of the pipe (causing the
    /// reader to see EOF and exit), joins the reader thread, and best-
    /// effort deletes the spill file.
    pub(super) struct HostCaptureImpl {
        terminal_stdout_fd: OwnedFd,
        terminal_stderr_fd: OwnedFd,
        /// One extra reference to the pipe write end retained so the
        /// pipe stays open even if some misbehaving code closes fd 1/2.
        /// Wrapped in `Option` so the shutdown sequence can `take()` it
        /// out and drop it explicitly before joining the reader.
        retained_write_end: Option<OwnedFd>,
        reader: Option<JoinHandle<()>>,
        spill_path: PathBuf,
        /// Process-start instant captured at install time. Lines in the
        /// spill file are recorded as `Duration` offsets from this base
        /// so the reader thread can use monotonic time and the consumer
        /// side can correlate them with per-test windows.
        epoch: Instant,
        /// Wall-clock equivalent of `epoch` captured at install time.
        /// Storing both pinned avoids reconstructing the wall epoch via
        /// `SystemTime::now() - epoch.elapsed()` at finalize time, which
        /// would drift if the system clock jumped between install and
        /// finalize.
        epoch_wall: std::time::SystemTime,
        /// `true` once [`finalize_in_place`] has run the teardown.
        /// `Drop` becomes a no-op for the teardown half in that case but
        /// always best-effort deletes the spill file (in case `finalize`
        /// returned without removing it for some reason).
        finalized: bool,
    }

    pub(super) fn install(_args: &Arguments) -> io::Result<HostCaptureImpl> {
        // ----- Phase 1: all fallible setup that DOES NOT touch fd 1/2 -----

        // Duplicate fd 1 and fd 2 *before* we redirect them so the
        // formatter still has a path to the real terminal and rollback
        // (in Phase 2) has somewhere to point fd 1/2 back to.
        let terminal_stdout_fd = dup_owned(libc::STDOUT_FILENO)?;
        let terminal_stderr_fd = dup_owned(libc::STDERR_FILENO)?;
        // Internal fds; do not leak into child processes.
        set_cloexec(terminal_stdout_fd.as_raw_fd())?;
        set_cloexec(terminal_stderr_fd.as_raw_fd())?;

        // Create the host-side capture pipe with both ends CLOEXEC so
        // spawned worker subprocesses don't inherit them — that would
        // keep `retained_write_end` alive across processes and the
        // reader thread would never see EOF at suite end.
        let (read_end, write_end) = make_pipe_cloexec()?;

        // Spill file: <tmpdir>/test-r-host-log-<uuid>.bin
        let spill_path =
            std::env::temp_dir().join(format!("test-r-host-log-{}.bin", Uuid::new_v4()));
        let spill_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&spill_path)?;
        let spill_file = Arc::new(Mutex::new(spill_file));

        // Capture the monotonic + wall epochs at install time. They
        // form the basis for per-test `HostWindow`s and for the
        // wall-clock timestamps surfaced on `CapturedOutput::Host`.
        let epoch = Instant::now();
        let epoch_wall = std::time::SystemTime::now();

        // From here on, any fallible step must remove the spill file on
        // failure: it exists on disk and would otherwise be leaked.
        // Wrap the remaining phase-1 steps in a closure so a single
        // `?` propagation point is responsible for cleanup.
        let setup = || -> io::Result<JoinHandle<()>> {
            // Install the terminal-fd accessor BEFORE the redirect so
            // any formatter call between Phase 2 and the function
            // return still finds a path to the real terminal.
            install_terminal_fds(&terminal_stdout_fd, &terminal_stderr_fd)?;

            // Spawn the reader BEFORE redirecting fd 1/2 so writes
            // into the pipe never block on a full kernel buffer with
            // no reader. If the redirect below fails the JoinHandle is
            // dropped (detached) and the reader exits as soon as
            // `write_end` is dropped at function return.
            let read_end_file = unsafe { File::from_raw_fd(read_end.into_raw_fd()) };
            let spill_clone = spill_file.clone();
            let epoch_clone = epoch;
            let reader = thread::Builder::new()
                .name("test-r-host-capture".to_string())
                .spawn(move || {
                    reader_loop(read_end_file, spill_clone, epoch_clone);
                })?;
            Ok(reader)
        };

        let reader = match setup() {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_file(&spill_path);
                return Err(e);
            }
        };

        // ----- Phase 2: the redirect itself, with explicit rollback -----

        // First dup2: if this fails, fd 1/2 are still unchanged. Just
        // clean up the spill file before returning; the local
        // `write_end` drops next, the reader exits, and the
        // `terminal_*_fd` handles get closed.
        if let Err(e) = dup2_overwrite(write_end.as_raw_fd(), libc::STDOUT_FILENO) {
            let _ = std::fs::remove_file(&spill_path);
            return Err(e);
        }
        // Second dup2: if this fails, fd 1 is already redirected — put
        // it back from the saved terminal fd before returning so the
        // process doesn't end up writing into a pipe with a detached
        // reader.
        if let Err(e) = dup2_overwrite(write_end.as_raw_fd(), libc::STDERR_FILENO) {
            let _ = dup2_overwrite(terminal_stdout_fd.as_raw_fd(), libc::STDOUT_FILENO);
            let _ = std::fs::remove_file(&spill_path);
            return Err(e);
        }

        Ok(HostCaptureImpl {
            terminal_stdout_fd,
            terminal_stderr_fd,
            retained_write_end: Some(write_end),
            reader: Some(reader),
            spill_path,
            epoch,
            epoch_wall,
            finalized: false,
        })
    }

    impl HostCaptureImpl {
        pub fn spill_path(&self) -> &std::path::Path {
            &self.spill_path
        }

        pub fn epoch(&self) -> Instant {
            self.epoch
        }

        pub fn epoch_wall(&self) -> std::time::SystemTime {
            self.epoch_wall
        }

        /// Shared shutdown path for `finalize` and `Drop`.
        ///
        /// 1. restores fd 1/2 from the saved terminal fds (so any
        ///    further prints — including from `_hosted_owners`' Drop
        ///    impls — reach the terminal, not the about-to-close pipe);
        /// 2. tears down the terminal-fd accessor;
        /// 3. drops the retained write end, which is the last open write
        ///    end of the pipe at this point, so the reader sees EOF;
        /// 4. joins the reader thread so the spill file is fully flushed
        ///    before any subsequent read.
        ///
        /// Idempotent: re-entry is a no-op.
        fn shutdown_pipe(&mut self) {
            if self.finalized {
                return;
            }
            // Flush any buffered Rust stdio writes BEFORE we point
            // fd 1/2 back at the real terminal, so a partial `print!`
            // without trailing newline doesn't end up on the terminal
            // instead of inside the host-capture stream.
            let _ = io::stdout().flush();
            let _ = io::stderr().flush();
            let _ = dup2_overwrite(self.terminal_stdout_fd.as_raw_fd(), libc::STDOUT_FILENO);
            let _ = dup2_overwrite(self.terminal_stderr_fd.as_raw_fd(), libc::STDERR_FILENO);
            clear_terminal_fds();
            drop(self.retained_write_end.take());
            if let Some(handle) = self.reader.take() {
                let _ = handle.join();
            }
            self.finalized = true;
        }

        /// Performs the shutdown teardown and then parses the spill
        /// file into records. The spill file is deleted on success
        /// (and best-effort on error); on parse failure the partial
        /// vec collected so far is returned.
        pub fn finalize_in_place(&mut self) -> Vec<super::HostLogRecord> {
            self.shutdown_pipe();
            let records = read_spill_file(&self.spill_path).unwrap_or_default();
            let _ = std::fs::remove_file(&self.spill_path);
            records
        }
    }

    impl Drop for HostCaptureImpl {
        fn drop(&mut self) {
            self.shutdown_pipe();
            // Best-effort delete of the spill file. Already deleted by
            // a prior `finalize_in_place` call in the common path.
            let _ = std::fs::remove_file(&self.spill_path);
        }
    }

    fn dup_owned(fd: RawFd) -> io::Result<OwnedFd> {
        // SAFETY: `libc::dup` returns a new fd on success or -1 on
        // error. We claim ownership via `OwnedFd::from_raw_fd` only on
        // success.
        let new_fd = unsafe { libc::dup(fd) };
        if new_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(new_fd) })
    }

    /// `dup_owned` + `set_cloexec`. Used for internal fd duplicates
    /// (terminal-fd statics, etc.) that must not be inherited by
    /// worker subprocesses.
    pub(super) fn dup_owned_cloexec(fd: RawFd) -> io::Result<OwnedFd> {
        let new = dup_owned(fd)?;
        set_cloexec(new.as_raw_fd())?;
        Ok(new)
    }

    fn dup2_overwrite(src: RawFd, dst: RawFd) -> io::Result<()> {
        // SAFETY: `libc::dup2` returns dst on success or -1 on error.
        // Both fds remain owned by their respective handles (we're
        // copying, not transferring ownership). EINTR is retried so a
        // signal raised while the kernel was duplicating the fd doesn't
        // leave fd 1/2 half-redirected.
        loop {
            let rc = unsafe { libc::dup2(src, dst) };
            if rc >= 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// Sets `FD_CLOEXEC` on `fd` so the descriptor is not inherited by
    /// child processes. The host-capture pipe ends and the saved
    /// terminal-fd duplicates are internal to the parent test runner;
    /// inheriting them by a spawned worker would either leak host
    /// stdout into the worker (rare) or, worse, keep the pipe write
    /// end open after the parent's reader expects EOF, hanging
    /// `HostCapture::finalize`.
    fn set_cloexec(fd: RawFd) -> io::Result<()> {
        // SAFETY: `fcntl(F_GETFD)` reads existing flags; `F_SETFD`
        // sets them. Both return -1 on error.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Creates an unnamed pipe with `FD_CLOEXEC` set on both ends.
    /// Older platforms without `pipe2` fall back to `pipe + fcntl`,
    /// which has a small window between syscalls where a concurrent
    /// `fork+exec` could inherit the fd; we accept that on the
    /// fallback path since test-r workers spawn from a single, serial
    /// place far from `install`.
    fn make_pipe_cloexec() -> io::Result<(OwnedFd, OwnedFd)> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `pipe2` writes two fds into `fds` on success or
        // returns -1 on error. Not available on macOS — fall back
        // there to `pipe + fcntl`.
        #[cfg(any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly",
            target_os = "illumos",
            target_os = "solaris",
        ))]
        {
            let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly",
            target_os = "illumos",
            target_os = "solaris",
        )))]
        {
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            // Best-effort CLOEXEC; if fcntl fails the worst case is an
            // inherited internal fd, not a deadlock, so we don't unwind.
            let _ = set_cloexec(fds[0]);
            let _ = set_cloexec(fds[1]);
            Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
        }
    }

    /// Reader thread body: line-buffer the pipe and spill records to
    /// `spill_file`. Exits on EOF (when the last write end is closed)
    /// or on unrecoverable I/O error.
    ///
    /// Record format (little-endian):
    ///   u64 nanoseconds-since-epoch
    ///   u8  stream tag (0 = mixed; reserved for future split)
    ///   u32 byte length
    ///   N   raw bytes (no trailing newline)
    fn reader_loop(read_end: File, spill_file: Arc<Mutex<File>>, epoch: Instant) {
        let mut reader = BufReader::with_capacity(64 * 1024, read_end);
        let mut line = Vec::with_capacity(256);
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break, // EOF — all write ends closed
                Ok(_n) => {
                    // Strip the trailing '\n' (and a preceding '\r' if
                    // present) so the record is just the line bytes.
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    let elapsed = epoch.elapsed().as_nanos();
                    let ts_ns = u64::try_from(elapsed).unwrap_or(u64::MAX);
                    let stream_tag: u8 = 0;
                    let len = u32::try_from(line.len()).unwrap_or(u32::MAX) as usize;
                    let len_u32 = len as u32;

                    let mut header = [0u8; 8 + 1 + 4];
                    header[..8].copy_from_slice(&ts_ns.to_le_bytes());
                    header[8] = stream_tag;
                    header[9..13].copy_from_slice(&len_u32.to_le_bytes());

                    if let Ok(mut f) = spill_file.lock() {
                        let _ = f.write_all(&header);
                        let _ = f.write_all(&line[..len]);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }

        // Flush the spill file so subsequent readers see all records.
        if let Ok(mut f) = spill_file.lock() {
            let _ = f.flush();
        }
    }

    /// Parses the spill file written by [`reader_loop`] into a vec of
    /// [`super::HostLogRecord`]s in file order (which is also temporal
    /// order, since records are appended as they arrive).
    ///
    /// On a truncated trailing record (e.g. the reader was killed mid-
    /// write) the truncated tail is silently dropped and the records
    /// parsed so far are returned. A missing file returns an empty vec
    /// rather than an error, since "no host output at all" is the most
    /// common and least surprising outcome.
    pub(super) fn read_spill_file(path: &super::Path) -> io::Result<Vec<super::HostLogRecord>> {
        use std::io::Read;

        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        let mut out = Vec::new();
        let mut pos = 0;
        while pos + 13 <= bytes.len() {
            let mut ts_buf = [0u8; 8];
            ts_buf.copy_from_slice(&bytes[pos..pos + 8]);
            let elapsed_ns = u64::from_le_bytes(ts_buf);

            let stream_tag = bytes[pos + 8];

            let mut len_buf = [0u8; 4];
            len_buf.copy_from_slice(&bytes[pos + 9..pos + 13]);
            let line_len = u32::from_le_bytes(len_buf) as usize;

            pos += 13;
            if pos + line_len > bytes.len() {
                // Truncated tail — stop parsing rather than panic.
                break;
            }
            let line_bytes = &bytes[pos..pos + line_len];
            pos += line_len;

            out.push(super::HostLogRecord {
                elapsed: super::Duration::from_nanos(elapsed_ns),
                stream_tag,
                line: String::from_utf8_lossy(line_bytes).into_owned(),
            });
        }
        Ok(out)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::fs::File;
    use std::io::{BufRead, BufReader};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Instant;
    use uuid::Uuid;
    use windows_sys::Win32::Foundation::{
        DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// Active capture handle on Windows. Drop restores the original
    /// `STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE` values, closes the held
    /// write end of the pipe (causing the reader to see `BROKEN_PIPE`
    /// and exit), joins the reader thread, and best-effort deletes the
    /// spill file.
    pub(super) struct HostCaptureImpl {
        /// Saved values of the OS-level stdio slots. These are raw
        /// handle pointers we DO NOT own — `SetStdHandle` only swaps
        /// pointers, it does not change ownership. We restore them on
        /// teardown.
        original_stdout: HANDLE,
        original_stderr: HANDLE,
        /// One reference to the pipe write end retained so the pipe
        /// stays open even if some misbehaving code closes the swapped
        /// `STD_*_HANDLE`. Wrapped in `Option` so shutdown can `take()`
        /// it out and drop it before joining the reader.
        retained_write_end: Option<OwnedHandle>,
        reader: Option<JoinHandle<()>>,
        spill_path: PathBuf,
        epoch: Instant,
        epoch_wall: std::time::SystemTime,
        finalized: bool,
    }

    pub(super) fn install(_args: &Arguments) -> io::Result<HostCaptureImpl> {
        // ----- Phase 1: setup that DOES NOT touch STD_*_HANDLE -----

        // Snapshot the current STD handles so we can restore them on
        // teardown. `GetStdHandle` returns the raw value stored in the
        // OS stdio slot; we do not own it and must not close it.
        let original_stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if original_stdout == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let original_stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if original_stderr == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        // Create an anonymous pipe. With null `SECURITY_ATTRIBUTES`,
        // the returned handles are NOT inheritable, so worker
        // subprocesses we later spawn cannot inherit the write end and
        // hold it open past suite end (which would prevent the reader
        // from seeing EOF and hang `finalize`).
        let mut read_end: HANDLE = std::ptr::null_mut();
        let mut write_end: HANDLE = std::ptr::null_mut();
        let rc = unsafe { CreatePipe(&mut read_end, &mut write_end, std::ptr::null(), 0) };
        if rc == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `CreatePipe` returned two fresh kernel handles owned
        // by us; wrap each in `OwnedHandle` so RAII closes them on any
        // error path below.
        let read_end = unsafe { OwnedHandle::from_raw_handle(read_end as RawHandle) };
        let write_end = unsafe { OwnedHandle::from_raw_handle(write_end as RawHandle) };

        // Spill file: <tmpdir>/test-r-host-log-<uuid>.bin
        let spill_path =
            std::env::temp_dir().join(format!("test-r-host-log-{}.bin", Uuid::new_v4()));
        let spill_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&spill_path)?;
        let spill_file = Arc::new(Mutex::new(spill_file));

        // Capture the monotonic + wall epochs at install time. They
        // form the basis for per-test `HostWindow`s and for the
        // wall-clock timestamps surfaced on `CapturedOutput::Host`.
        let epoch = Instant::now();
        let epoch_wall = std::time::SystemTime::now();

        // From here on, any fallible step must remove the spill file on
        // failure; centralise that via a closure.
        let setup = || -> io::Result<JoinHandle<()>> {
            install_terminal_handles(original_stdout, original_stderr)?;

            // Spawn the reader BEFORE redirecting STD_*_HANDLE so writes
            // into the pipe never block on a full pipe buffer with no
            // reader. If the redirect below fails the `JoinHandle` is
            // dropped (detached) and the reader exits as soon as
            // `write_end` is dropped at function return.
            let read_end_file = File::from(read_end);
            let spill_clone = spill_file.clone();
            let epoch_clone = epoch;
            let reader = thread::Builder::new()
                .name("test-r-host-capture".to_string())
                .spawn(move || {
                    reader_loop(read_end_file, spill_clone, epoch_clone);
                })?;
            Ok(reader)
        };

        let reader = match setup() {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_file(&spill_path);
                return Err(e);
            }
        };

        // ----- Phase 2: redirect, with explicit rollback -----

        // Helper to drop the write end, join the reader so it stops
        // holding the spill file open, and then best-effort delete the
        // spill file. On Windows you cannot delete a file that is still
        // open in this process, so the join must come BEFORE the
        // `remove_file` call or the spill file would leak in temp.
        let cleanup_on_failure = |write_end: OwnedHandle, reader: JoinHandle<()>| {
            drop(write_end);
            let _ = reader.join();
            let _ = std::fs::remove_file(&spill_path);
        };

        let write_handle: HANDLE = write_end.as_raw_handle() as HANDLE;
        if unsafe { SetStdHandle(STD_OUTPUT_HANDLE, write_handle) } == 0 {
            let e = io::Error::last_os_error();
            cleanup_on_failure(write_end, reader);
            return Err(e);
        }
        if unsafe { SetStdHandle(STD_ERROR_HANDLE, write_handle) } == 0 {
            let e = io::Error::last_os_error();
            // Roll STD_OUTPUT_HANDLE back to the saved terminal value
            // before we bail so the process doesn't end up writing into
            // a pipe with a detached reader.
            unsafe {
                let _ = SetStdHandle(STD_OUTPUT_HANDLE, original_stdout);
            }
            cleanup_on_failure(write_end, reader);
            return Err(e);
        }

        Ok(HostCaptureImpl {
            original_stdout,
            original_stderr,
            retained_write_end: Some(write_end),
            reader: Some(reader),
            spill_path,
            epoch,
            epoch_wall,
            finalized: false,
        })
    }

    impl HostCaptureImpl {
        pub fn spill_path(&self) -> &Path {
            &self.spill_path
        }

        pub fn epoch(&self) -> Instant {
            self.epoch
        }

        pub fn epoch_wall(&self) -> std::time::SystemTime {
            self.epoch_wall
        }

        /// Shared shutdown path for `finalize` and `Drop`. Idempotent.
        fn shutdown_pipe(&mut self) {
            if self.finalized {
                return;
            }
            // Flush any buffered Rust stdio writes BEFORE we point
            // STD_*_HANDLE back at the real terminal, so a partial
            // `print!` without trailing newline doesn't end up on the
            // terminal instead of inside the host-capture stream.
            let _ = io::stdout().flush();
            let _ = io::stderr().flush();
            unsafe {
                let _ = SetStdHandle(STD_OUTPUT_HANDLE, self.original_stdout);
                let _ = SetStdHandle(STD_ERROR_HANDLE, self.original_stderr);
            }
            clear_terminal_handles();
            drop(self.retained_write_end.take());
            if let Some(handle) = self.reader.take() {
                let _ = handle.join();
            }
            self.finalized = true;
        }

        pub fn finalize_in_place(&mut self) -> Vec<super::HostLogRecord> {
            self.shutdown_pipe();
            let records = read_spill_file(&self.spill_path).unwrap_or_default();
            let _ = std::fs::remove_file(&self.spill_path);
            records
        }
    }

    impl Drop for HostCaptureImpl {
        fn drop(&mut self) {
            self.shutdown_pipe();
            let _ = std::fs::remove_file(&self.spill_path);
        }
    }

    /// Duplicates a raw Win32 `HANDLE` into a fresh, non-inheritable
    /// `OwnedHandle` that closes via `CloseHandle` when dropped.
    ///
    /// The original `HANDLE` returned from `GetStdHandle` is NOT owned
    /// by us — it is the kernel's per-process stdio slot value. Calling
    /// `CloseHandle` on it would close stdio for the whole process.
    /// Duplicating gives us a separate handle the formatter statics can
    /// own and free safely.
    pub(super) fn duplicate_handle_owned(h: HANDLE) -> io::Result<OwnedHandle> {
        let process = unsafe { GetCurrentProcess() };
        let mut new_handle: HANDLE = std::ptr::null_mut();
        let rc = unsafe {
            DuplicateHandle(
                process,
                h,
                process,
                &mut new_handle,
                0,
                0, // bInheritHandle: FALSE — formatter handles must not
                // leak into worker subprocesses.
                DUPLICATE_SAME_ACCESS,
            )
        };
        if rc == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `DuplicateHandle` filled `new_handle` with a fresh
        // kernel handle owned by us; wrap it in `OwnedHandle` so it is
        // closed via `CloseHandle` on drop.
        Ok(unsafe { OwnedHandle::from_raw_handle(new_handle as RawHandle) })
    }

    /// Reader thread body: line-buffer the pipe and spill records to
    /// `spill_file`. Exits on EOF (when the last write end is closed)
    /// or on unrecoverable I/O error.
    ///
    /// Identical record format to the Unix reader (see Unix `imp`).
    fn reader_loop(read_end: File, spill_file: Arc<Mutex<File>>, epoch: Instant) {
        let mut reader = BufReader::with_capacity(64 * 1024, read_end);
        let mut line = Vec::with_capacity(256);
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break, // EOF — all write ends closed
                Ok(_) => {
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    let elapsed = epoch.elapsed().as_nanos();
                    let ts_ns = u64::try_from(elapsed).unwrap_or(u64::MAX);
                    let stream_tag: u8 = 0;
                    let len = u32::try_from(line.len()).unwrap_or(u32::MAX) as usize;
                    let len_u32 = len as u32;
                    let mut header = [0u8; 8 + 1 + 4];
                    header[..8].copy_from_slice(&ts_ns.to_le_bytes());
                    header[8] = stream_tag;
                    header[9..13].copy_from_slice(&len_u32.to_le_bytes());
                    if let Ok(mut f) = spill_file.lock() {
                        let _ = f.write_all(&header);
                        let _ = f.write_all(&line[..len]);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // BROKEN_PIPE is the Windows equivalent of EOF here
                // (all write ends closed). Treat it as a normal exit.
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break,
                Err(_) => break,
            }
        }
        if let Ok(mut f) = spill_file.lock() {
            let _ = f.flush();
        }
    }

    /// Parses the spill file written by [`reader_loop`] into a vec of
    /// [`super::HostLogRecord`]s in file order. Identical layout to the
    /// Unix reader; truncated trailing records are silently dropped.
    pub(super) fn read_spill_file(path: &super::Path) -> io::Result<Vec<super::HostLogRecord>> {
        use std::io::Read;
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        let mut out = Vec::new();
        let mut pos = 0;
        while pos + 13 <= bytes.len() {
            let mut ts_buf = [0u8; 8];
            ts_buf.copy_from_slice(&bytes[pos..pos + 8]);
            let elapsed_ns = u64::from_le_bytes(ts_buf);
            let stream_tag = bytes[pos + 8];
            let mut len_buf = [0u8; 4];
            len_buf.copy_from_slice(&bytes[pos + 9..pos + 13]);
            let line_len = u32::from_le_bytes(len_buf) as usize;
            pos += 13;
            if pos + line_len > bytes.len() {
                break;
            }
            let line_bytes = &bytes[pos..pos + line_len];
            pos += line_len;
            out.push(super::HostLogRecord {
                elapsed: super::Duration::from_nanos(elapsed_ns),
                stream_tag,
                line: String::from_utf8_lossy(line_bytes).into_owned(),
            });
        }
        Ok(out)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;
    use std::path::Path;
    use std::time::Instant;

    /// No-op handle on targets we don't yet support. Host-side prints
    /// continue to reach stdout/stderr directly (the pre-existing
    /// behaviour), and the formatter accessors below transparently
    /// fall back to `io::stdout()` / `io::stderr()`.
    pub(super) struct HostCaptureImpl;

    pub(super) fn install(_args: &Arguments) -> io::Result<HostCaptureImpl> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "test-r host-side output capture is not supported on this target",
        ))
    }

    impl HostCaptureImpl {
        pub fn spill_path(&self) -> &Path {
            Path::new("")
        }

        pub fn epoch(&self) -> Instant {
            Instant::now()
        }

        pub fn epoch_wall(&self) -> std::time::SystemTime {
            std::time::SystemTime::now()
        }

        pub fn finalize_in_place(&mut self) -> Vec<super::HostLogRecord> {
            Vec::new()
        }
    }
}

/// Public handle returned by [`install_if_needed`]. Drop tears down the
/// capture and restores fd 1/2 to the terminal.
pub(crate) struct HostCapture {
    inner: imp::HostCaptureImpl,
}

impl HostCapture {
    pub(crate) fn spill_path(&self) -> &std::path::Path {
        self.inner.spill_path()
    }

    pub(crate) fn epoch(&self) -> std::time::Instant {
        self.inner.epoch()
    }

    /// Wall-clock timestamp pinned at install time. Used by
    /// [`attribute_records_to_tests`] to convert a record's monotonic
    /// `elapsed` offset into a stable `SystemTime` for sorting against
    /// the per-test stdout/stderr captures.
    pub(crate) fn epoch_wall(&self) -> std::time::SystemTime {
        self.inner.epoch_wall()
    }

    /// Stops the capture, joins the reader thread, parses the spill
    /// file into [`HostLogRecord`]s and returns them in temporal order.
    /// Restores fd 1/2 to the original terminal as part of the
    /// shutdown.
    ///
    /// After `finalize`, all subsequent host-side prints in this
    /// process go straight to the terminal again.
    pub(crate) fn finalize(mut self) -> Vec<HostLogRecord> {
        self.inner.finalize_in_place()
    }
}

/// Half-open per-test execution window expressed as elapsed-ns offsets
/// from [`HostCapture::epoch`]. Used by the suite runners to attribute
/// host-log records to the test(s) whose window contained the record.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HostWindow {
    pub start: Duration,
    pub end: Duration,
}

impl HostWindow {
    /// Returns the half-open window `[start, end)` from an `Instant`-
    /// indexed pair, relative to the capture epoch. `None` when no
    /// capture is installed (the caller has no need to attribute).
    pub(crate) fn from_instants(
        epoch: Option<std::time::Instant>,
        start: std::time::Instant,
        end: std::time::Instant,
    ) -> Option<Self> {
        let epoch = epoch?;
        Some(Self {
            start: start.saturating_duration_since(epoch),
            end: end.saturating_duration_since(epoch),
        })
    }

    fn contains(&self, t: Duration) -> bool {
        t >= self.start && t < self.end
    }
}

/// Converts a [`HostLogRecord`] into a [`crate::internal::CapturedOutput`]
/// marked as `Host`-origin, using `epoch + record.elapsed` as the
/// `SystemTime` for ordering against the test's own stdout/stderr
/// captures.
pub(crate) fn record_to_capture(
    epoch_wall: std::time::SystemTime,
    rec: &HostLogRecord,
) -> crate::internal::CapturedOutput {
    let ts = epoch_wall.checked_add(rec.elapsed).unwrap_or(epoch_wall);
    crate::internal::CapturedOutput::host(ts, rec.line.clone())
}

/// Attributes parsed [`HostLogRecord`]s to per-test windows.
///
/// For each `(test, window)` pair, every record whose `elapsed` lies
/// inside `window` is appended (sorted by timestamp) to that test's
/// `captured_output` vec, tagged as
/// [`CapturedOutput::Host`](crate::internal::CapturedOutput::Host).
/// A single record can be attributed to multiple tests when their
/// windows overlap (parallel execution). Records that don't fall in
/// any window are silently dropped for now — surfacing them out-of-
/// band is a follow-up step.
///
/// `epoch_wall` must be the wall-clock timestamp pinned by
/// [`HostCapture::epoch_wall`] at install time. Reconstructing it from
/// `SystemTime::now()` and `Instant::elapsed()` at attribution time
/// would drift if the wall clock jumped between install and finalize,
/// so callers must pass the pinned value through.
pub(crate) fn attribute_records_to_tests(
    epoch_wall: std::time::SystemTime,
    records: &[HostLogRecord],
    windows: &[(usize, HostWindow)],
    results: &mut [(crate::internal::RegisteredTest, crate::internal::TestResult)],
) {
    if records.is_empty() || windows.is_empty() {
        return;
    }
    for (test_idx, win) in windows {
        // Collect records that fall inside this test's window.
        let mut additions: Vec<crate::internal::CapturedOutput> = records
            .iter()
            .filter(|r| win.contains(r.elapsed))
            .map(|r| record_to_capture(epoch_wall, r))
            .collect();
        if additions.is_empty() {
            continue;
        }
        // Merge into the existing captured vec, keeping the existing
        // sort-by-timestamp invariant (`Ord for CapturedOutput`).
        let Some((_, result)) = results.get_mut(*test_idx) else {
            continue;
        };
        let mut merged = result.captured_output().clone();
        merged.append(&mut additions);
        merged.sort();
        result.set_captured_output(merged);
    }
}

/// Install host-side output capture if this process is a top-level
/// parent that is going to spawn worker subprocesses for capture.
///
/// Returns `None` when:
/// - this is an IPC worker subprocess (own stdout/stderr is the parent
///   pipe — we must not redirect or the parent's drain stops working);
/// - `--nocapture` is set (we want everything to keep going directly to
///   the terminal);
/// - this attempt won't actually spawn workers (so there is no
///   structured per-test capture to align host output against);
/// - the target is neither Unix nor Windows (other targets fall back
///   to a no-op stub);
/// - install failed for any I/O reason (we silently fall back to the
///   pre-existing behaviour so a broken capture never breaks the run).
///
/// Callers should invoke this AFTER
/// [`crate::args::Arguments::finalize_for_execution`] so that
/// `args.spawn_workers` reflects the actual decision for this attempt.
pub(crate) fn install_if_needed(args: &Arguments) -> Option<HostCapture> {
    // Snapshot the real terminal status of fd 1/2 BEFORE any redirect
    // happens, so the formatters keep getting the right answer about
    // colour / OSC support after the host pipe takes over fd 1/2.
    // We do this unconditionally so workers and --nocapture runs also
    // get an accurate cached answer rather than a half-initialised one.
    let _ = REAL_STDOUT_IS_TERMINAL.set(io::stdout().is_terminal());
    let _ = REAL_STDERR_IS_TERMINAL.set(io::stderr().is_terminal());

    if args.ipc.is_some() {
        return None;
    }
    if args.nocapture {
        return None;
    }
    if !args.spawn_workers {
        // No worker subprocesses to spawn ⇒ the entire suite runs
        // in-process, so the existing per-test stdout/stderr capture
        // already covers everything the user code emits and there is
        // no parent/worker split for host capture to bridge. Skip the
        // fd redirect entirely.
        return None;
    }
    match imp::install(args) {
        Ok(inner) => Some(HostCapture { inner }),
        Err(_) => None,
    }
}

// --------------------------------------------------------------------
// Terminal-fd accessors used by the formatters.
//
// Formatters call `with_terminal_stdout`/`with_terminal_stderr` (or
// construct a `TerminalStdout`/`TerminalStderr` for use anywhere a
// `Write` is expected). When host capture is active these route to the
// real terminal via the dup'd fds installed during `install`; otherwise
// they transparently delegate to `io::stdout()` / `io::stderr()`.
// --------------------------------------------------------------------

#[cfg(unix)]
static TERMINAL_STDOUT: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
#[cfg(unix)]
static TERMINAL_STDERR: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

#[cfg(unix)]
fn install_terminal_fds(
    stdout_fd: &std::os::fd::OwnedFd,
    stderr_fd: &std::os::fd::OwnedFd,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // Skip if the statics are already populated (e.g. a previous
    // retry attempt installed them and we don't want to leak a fresh
    // pair of fds whose `OnceLock::set` would be rejected). The
    // existing terminal fds still point at the real fd 1/2 that we
    // just dup'd into `stdout_fd`/`stderr_fd`, so this is correct.
    if TERMINAL_STDOUT.get().is_some() && TERMINAL_STDERR.get().is_some() {
        return Ok(());
    }
    // Re-dup each side so the static handle owns its own fd and won't
    // be closed when `HostCapture` is dropped (the saved
    // terminal_*_fd OwnedFds are restored over fd 1/2 first; after
    // that we want the statics gone too — see `clear_terminal_fds`).
    // Both static dups are marked CLOEXEC so they do not get
    // inherited by worker subprocesses.
    use imp::dup_owned_cloexec;
    let stdout_owned = dup_owned_cloexec(stdout_fd.as_raw_fd())?;
    let stderr_owned = match dup_owned_cloexec(stderr_fd.as_raw_fd()) {
        Ok(fd) => fd,
        Err(e) => {
            // First dup succeeded but second failed: drop the first so
            // the fd is closed and not leaked.
            drop(stdout_owned);
            return Err(e);
        }
    };
    let stdout_file = std::fs::File::from(stdout_owned);
    let stderr_file = std::fs::File::from(stderr_owned);
    // OnceLock::set returns the value back as Err if already set;
    // in that case our just-dup'd fds would leak. Drop the rejected
    // wrappers so their owned File closes the fd properly.
    if let Err(rejected) = TERMINAL_STDOUT.set(Mutex::new(stdout_file)) {
        drop(rejected);
    }
    if let Err(rejected) = TERMINAL_STDERR.set(Mutex::new(stderr_file)) {
        drop(rejected);
    }
    Ok(())
}

#[cfg(unix)]
fn clear_terminal_fds() {
    // OnceLock has no `take` API on stable, so the statics stay
    // populated for the rest of the process. After Drop, fd 1/2 are
    // restored to the real terminal, and `TerminalStdout` / `TerminalStderr`
    // continue to write to that same terminal via the dup'd fds the
    // statics hold — which is exactly what we want for any late prints.
}

#[cfg(windows)]
static TERMINAL_STDOUT: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
#[cfg(windows)]
static TERMINAL_STDERR: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

#[cfg(windows)]
fn install_terminal_handles(
    stdout_handle: windows_sys::Win32::Foundation::HANDLE,
    stderr_handle: windows_sys::Win32::Foundation::HANDLE,
) -> io::Result<()> {
    // Skip if already installed by an earlier retry attempt — the
    // existing duplicated handles still target the real terminal that
    // the `original_stdout` / `original_stderr` raw values we were just
    // handed also point at.
    if TERMINAL_STDOUT.get().is_some() && TERMINAL_STDERR.get().is_some() {
        return Ok(());
    }
    let stdout_owned = imp::duplicate_handle_owned(stdout_handle)?;
    let stderr_owned = match imp::duplicate_handle_owned(stderr_handle) {
        Ok(h) => h,
        Err(e) => {
            // First dup succeeded but second failed: drop the first so
            // the handle is closed and not leaked.
            drop(stdout_owned);
            return Err(e);
        }
    };
    // `File::from(OwnedHandle)` takes ownership; the resulting File
    // closes the handle via `CloseHandle` on drop. Writes go straight
    // through `WriteFile` against the original terminal handle without
    // touching `STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE`, which is
    // exactly what we need after the pipe has taken those slots over.
    let stdout_file = std::fs::File::from(stdout_owned);
    let stderr_file = std::fs::File::from(stderr_owned);
    if let Err(rejected) = TERMINAL_STDOUT.set(Mutex::new(stdout_file)) {
        drop(rejected);
    }
    if let Err(rejected) = TERMINAL_STDERR.set(Mutex::new(stderr_file)) {
        drop(rejected);
    }
    Ok(())
}

#[cfg(windows)]
fn clear_terminal_handles() {
    // Same rationale as the Unix `clear_terminal_fds`: `OnceLock` has
    // no stable `take` API, so the statics stay populated for the rest
    // of the process. After Drop, `STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE`
    // are restored, and the statics continue to hold their duplicates
    // of those original handles, so any late prints still reach the
    // real terminal.
}

/// `Write`-impl that targets the real terminal stdout: the dup'd
/// terminal fd when host capture is active, or `io::stdout()` otherwise.
#[derive(Clone, Copy, Default)]
pub(crate) struct TerminalStdout;

/// `Write`-impl that targets the real terminal stderr.
#[derive(Clone, Copy, Default)]
pub(crate) struct TerminalStderr;

impl Write for TerminalStdout {
    #[cfg(any(unix, windows))]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match TERMINAL_STDOUT.get() {
            Some(mtx) => mtx.lock().unwrap().write(buf),
            None => io::stdout().write(buf),
        }
    }

    #[cfg(any(unix, windows))]
    fn flush(&mut self) -> io::Result<()> {
        match TERMINAL_STDOUT.get() {
            Some(mtx) => mtx.lock().unwrap().flush(),
            None => io::stdout().flush(),
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stdout().write(buf)
    }

    #[cfg(not(any(unix, windows)))]
    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

impl Write for TerminalStderr {
    #[cfg(any(unix, windows))]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match TERMINAL_STDERR.get() {
            Some(mtx) => mtx.lock().unwrap().write(buf),
            None => io::stderr().write(buf),
        }
    }

    #[cfg(any(unix, windows))]
    fn flush(&mut self) -> io::Result<()> {
        match TERMINAL_STDERR.get() {
            Some(mtx) => mtx.lock().unwrap().flush(),
            None => io::stderr().flush(),
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stderr().write(buf)
    }

    #[cfg(not(any(unix, windows)))]
    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use crate::internal::{
        CapturedOutput, RegisteredTest, TestFunction, TestProperties, TestResult,
    };
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    /// Cheap `RegisteredTest` builder for the unit tests below. The
    /// run function is a no-op because the attribution code never
    /// invokes it.
    fn dummy_test(name: &str) -> RegisteredTest {
        RegisteredTest {
            name: name.to_string(),
            crate_name: "test_crate".to_string(),
            module_path: "test_module".to_string(),
            run: TestFunction::Sync(Arc::new(|_| Box::new(()))),
            props: TestProperties::default(),
            dependencies: None,
        }
    }

    /// Encodes a sequence of `(elapsed_ns, line)` records into the same
    /// binary layout that the host-capture reader thread writes, so the
    /// round-trip parser can be exercised without booting the real
    /// pipe/spill machinery.
    fn encode_records(items: &[(u64, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (ts_ns, line) in items {
            let len = u32::try_from(line.len()).unwrap();
            out.extend_from_slice(&ts_ns.to_le_bytes());
            out.push(0u8); // stream_tag = 0 ("mixed")
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(line.as_bytes());
        }
        out
    }

    #[test]
    fn read_spill_file_round_trips_records_in_order() {
        let dir = std::env::temp_dir().join(format!("test-r-host-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spill.bin");
        let bytes = encode_records(&[
            (1_000_000, "first line"),
            (5_000_000, "second line"),
            (9_999_999_999, "third line, large ts"),
        ]);
        std::fs::write(&path, bytes).unwrap();

        let records = imp::read_spill_file(&path).unwrap();
        assert_eq!(records.len(), 3, "all three records must parse");
        assert_eq!(records[0].elapsed, Duration::from_nanos(1_000_000));
        assert_eq!(records[0].line, "first line");
        assert_eq!(records[1].line, "second line");
        assert_eq!(records[2].elapsed, Duration::from_nanos(9_999_999_999));
        assert_eq!(records[2].line, "third line, large ts");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_spill_file_drops_truncated_tail() {
        let dir = std::env::temp_dir().join(format!("test-r-host-tr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spill.bin");

        let mut bytes = encode_records(&[(1_000, "complete")]);
        // Append a header that promises 64 bytes of payload but only
        // give 3 — emulates an in-flight write at process exit.
        bytes.extend_from_slice(&2_000u64.to_le_bytes());
        bytes.push(0u8);
        bytes.extend_from_slice(&64u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        std::fs::write(&path, bytes).unwrap();

        let records = imp::read_spill_file(&path).unwrap();
        assert_eq!(
            records.len(),
            1,
            "the truncated trailing record must be dropped, not panic"
        );
        assert_eq!(records[0].line, "complete");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_spill_file_missing_returns_empty_vec() {
        let path = std::env::temp_dir().join(format!(
            "test-r-host-nf-{}-does-not-exist.bin",
            uuid::Uuid::new_v4()
        ));
        let records = imp::read_spill_file(&path).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn attribute_records_to_tests_inserts_host_lines_for_matching_window() {
        // Pretend the suite started 1s ago and the only test ran
        // between t=200ms and t=400ms relative to that.
        let epoch_wall = SystemTime::now() - Duration::from_secs(1);
        let win_a = HostWindow {
            start: Duration::from_millis(200),
            end: Duration::from_millis(400),
        };
        let win_b = HostWindow {
            start: Duration::from_millis(500),
            end: Duration::from_millis(700),
        };

        let records = vec![
            HostLogRecord {
                elapsed: Duration::from_millis(100),
                stream_tag: 0,
                line: "before any test".to_string(),
            },
            HostLogRecord {
                elapsed: Duration::from_millis(250),
                stream_tag: 0,
                line: "during a".to_string(),
            },
            HostLogRecord {
                elapsed: Duration::from_millis(600),
                stream_tag: 0,
                line: "during b".to_string(),
            },
            HostLogRecord {
                elapsed: Duration::from_millis(900),
                stream_tag: 0,
                line: "after both tests".to_string(),
            },
        ];

        let a = dummy_test("a");
        let b = dummy_test("b");
        let mut results: Vec<(RegisteredTest, TestResult)> = vec![
            (a.clone(), TestResult::passed(Duration::from_millis(200))),
            (b.clone(), TestResult::passed(Duration::from_millis(200))),
        ];
        // Seed an existing stdout line on each test so we can assert
        // the host line lands AFTER it (timestamps are ordered).
        results[0]
            .1
            .set_captured_output(vec![CapturedOutput::Stdout {
                timestamp: SystemTime::UNIX_EPOCH,
                line: "from test a".to_string(),
            }]);
        results[1]
            .1
            .set_captured_output(vec![CapturedOutput::Stdout {
                timestamp: SystemTime::UNIX_EPOCH,
                line: "from test b".to_string(),
            }]);

        let windows_indexed = vec![(0usize, win_a), (1usize, win_b)];
        attribute_records_to_tests(epoch_wall, &records, &windows_indexed, &mut results);

        // Test A: should have its own line + the "during a" host line.
        let a_caps = results[0].1.captured_output();
        let a_host: Vec<&str> = a_caps
            .iter()
            .filter_map(|c| match c {
                CapturedOutput::Host { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            a_host,
            vec!["during a"],
            "test A must receive only the host record inside its window"
        );

        let b_caps = results[1].1.captured_output();
        let b_host: Vec<&str> = b_caps
            .iter()
            .filter_map(|c| match c {
                CapturedOutput::Host { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            b_host,
            vec!["during b"],
            "test B must receive only the host record inside its window"
        );

        // Sanity: the existing stdout line is still there.
        assert!(a_caps
            .iter()
            .any(|c| matches!(c, CapturedOutput::Stdout { line, .. } if line == "from test a")));
        assert!(b_caps
            .iter()
            .any(|c| matches!(c, CapturedOutput::Stdout { line, .. } if line == "from test b")));

        // Sanity: records outside any window are silently dropped.
        let _ = a;
        let _ = b;
    }

    #[test]
    fn attribute_records_to_tests_handles_overlapping_windows() {
        // Both tests' windows cover the same instant; the same record
        // must end up attributed to both.
        let epoch_wall = SystemTime::now() - Duration::from_secs(1);
        let overlap = HostWindow {
            start: Duration::from_millis(100),
            end: Duration::from_millis(800),
        };

        let records = vec![HostLogRecord {
            elapsed: Duration::from_millis(500),
            stream_tag: 0,
            line: "shared host line".to_string(),
        }];

        let mut results: Vec<(RegisteredTest, TestResult)> = vec![
            (
                dummy_test("a"),
                TestResult::passed(Duration::from_millis(700)),
            ),
            (
                dummy_test("b"),
                TestResult::passed(Duration::from_millis(700)),
            ),
        ];

        let windows_indexed = vec![(0usize, overlap), (1usize, overlap)];
        attribute_records_to_tests(epoch_wall, &records, &windows_indexed, &mut results);

        for (_, r) in &results {
            let host_lines: Vec<&str> = r
                .captured_output()
                .iter()
                .filter_map(|c| match c {
                    CapturedOutput::Host { line, .. } => Some(line.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                host_lines,
                vec!["shared host line"],
                "overlapping windows must each receive the same host record"
            );
        }
    }
}
