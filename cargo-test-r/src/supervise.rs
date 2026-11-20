//! One invocation clock and one owner for all spawned processes. Signal handlers
//! never inspect children: spawn publishes ownership before the loop checks flags.
use crate::run::BinaryOutcome;
use std::process::Command;
use std::time::{Duration, Instant};

pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let multiplier = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => return Err("expected an integer followed by ms, s, m, or h".into()),
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .map(Duration::from_millis)
        .ok_or_else(|| "duration is invalid or too large".into())
}

pub struct Supervisor {
    started: Instant,
    deadline: Option<Duration>,
    grace: Duration,
    platform: platform::Processes,
    pub cleanup_failed: bool,
}

impl Supervisor {
    pub fn new(
        started: Instant,
        deadline: Option<Duration>,
        grace: Duration,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            started,
            deadline,
            grace,
            platform: platform::Processes::new()?,
            cleanup_failed: false,
        })
    }

    pub fn stop_reason(&self) -> Option<BinaryOutcome> {
        if let Some(signal) = self.platform.signal() {
            Some(BinaryOutcome::Cancelled { signal })
        } else if self.deadline.is_some_and(|d| self.started.elapsed() >= d) {
            Some(BinaryOutcome::TimedOut)
        } else {
            None
        }
    }

    pub fn stop_if_requested(&mut self) -> Option<BinaryOutcome> {
        let reason = self.stop_reason()?;
        if let Err(error) = self.platform.cleanup(self.grace, reason.exit_code(), false) {
            eprintln!("Process cleanup failed: {error:#}");
            self.cleanup_failed = true;
        }
        self.stop_reason().or(Some(reason))
    }

    pub fn run(
        &mut self,
        command: &mut Command,
        cleanup: bool,
        mut events: Option<&mut crate::events::BinaryEvents<'_>>,
    ) -> BinaryOutcome {
        if let Some(reason) = self.stop_if_requested() {
            if let Some(events) = events.as_deref_mut() {
                events.stop(&reason);
            }
            return reason;
        }
        self.platform.prepare(command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return BinaryOutcome::LaunchFailed {
                    error: error.to_string(),
                };
            }
        };
        self.platform.track(child.id());
        if let Some(events) = events.as_deref_mut() {
            events.started(child.id());
        }
        loop {
            // Cancellation wins even when exit and the signal arrive together.
            if let Some(reason) = self.stop_reason() {
                if let Some(events) = events.as_deref_mut() {
                    events.stop(&reason);
                }
                if let Err(error) = self.platform.cleanup(self.grace, reason.exit_code(), false) {
                    eprintln!("Process cleanup failed: {error:#}");
                    self.cleanup_failed = true;
                    let _ = child.kill();
                }
                let _ = child.try_wait();
                return self.stop_reason().unwrap_or(reason);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if cleanup && let Err(error) = self.platform.cleanup(self.grace, 1, true) {
                        eprintln!("Process cleanup failed: {error:#}");
                        self.cleanup_failed = true;
                    }
                    if !cleanup {
                        self.platform.prune_groups();
                    }
                    // Cleanup is invocation work; it must not rewrite a binary
                    // that already exited. The caller checks invocation stop next.
                    return status.into();
                }
                Ok(None) => self.platform.wait(Duration::from_millis(20)),
                Err(error) => {
                    if let Err(error) = self.platform.cleanup(self.grace, 1, false) {
                        eprintln!("Process cleanup failed: {error:#}");
                        self.cleanup_failed = true;
                        let _ = child.kill();
                    }
                    let _ = child.try_wait();
                    return BinaryOutcome::LaunchFailed {
                        error: error.to_string(),
                    };
                }
            }
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::collections::HashSet;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::{net::UnixStream, process::CommandExt};
    use std::sync::{
        Arc,
        atomic::{AtomicI32, AtomicUsize, Ordering},
    };

    pub struct Processes {
        groups: Vec<i32>,
        signal: Arc<AtomicI32>,
        count: Arc<AtomicUsize>,
        wake: UnixStream,
        handlers: Vec<signal_hook::SigId>,
    }

    impl Processes {
        pub fn new() -> anyhow::Result<Self> {
            #[cfg(target_os = "linux")]
            if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let (wake, writer) = UnixStream::pair()?;
            wake.set_nonblocking(true)?;
            writer.set_nonblocking(true)?;
            let signal = Arc::new(AtomicI32::new(0));
            let count = Arc::new(AtomicUsize::new(0));
            let mut result = Self {
                groups: vec![],
                signal,
                count,
                wake,
                handlers: vec![],
            };
            for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                let signal = result.signal.clone();
                let count = result.count.clone();
                let writer = writer.try_clone()?;
                // Only lock-free atomics and a nonblocking write in the handler.
                result.handlers.push(unsafe {
                    signal_hook::low_level::register(sig, move || {
                        let _ = signal.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
                        count.fetch_add(1, Ordering::SeqCst);
                        let _ = (&writer).write(&[1]);
                    })?
                });
            }
            Ok(result)
        }
        pub fn signal(&self) -> Option<i32> {
            match self.signal.load(Ordering::SeqCst) {
                0 => None,
                sig => Some(sig),
            }
        }
        pub fn prepare(&self, cmd: &mut Command) {
            cmd.process_group(0);
        }
        pub fn track(&mut self, pid: u32) {
            self.groups.push(pid as i32);
        }
        pub fn prune_groups(&mut self) {
            self.groups.retain(|group| {
                (unsafe { libc::kill(-group, 0) }) == 0
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            });
        }
        pub fn wait(&mut self, duration: Duration) {
            let mut fd = libc::pollfd {
                fd: self.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe {
                libc::poll(
                    &mut fd,
                    1,
                    duration.as_millis().min(i32::MAX as u128) as i32,
                );
            }
            let _ = self.wake.read(&mut [0; 128]);
        }
        fn adopted(&self) -> anyhow::Result<Vec<i32>> {
            #[cfg(target_os = "linux")]
            {
                let children = std::fs::read_to_string(format!(
                    "/proc/self/task/{}/children",
                    std::process::id()
                ))?;
                Ok(children
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()?)
            }
            #[cfg(not(target_os = "linux"))]
            {
                Ok(vec![])
            }
        }
        fn send(&mut self, signal: i32, sent: &mut HashSet<(i32, i32)>) -> anyhow::Result<()> {
            self.prune_groups();
            let escaped = self.adopted()?.into_iter().filter(|pid| {
                // /proc lists direct children too. Do not deliver a second
                // interrupt to a process already covered by its process group.
                !self.groups.contains(&unsafe { libc::getpgid(*pid) })
            });
            for pid in self.groups.iter().map(|g| -g).chain(escaped) {
                // A process can fork while KILL is being delivered. Repeat KILL
                // during the drain, but never turn one interrupt into several.
                if signal != libc::SIGKILL && !sent.insert((pid, signal)) {
                    continue;
                }
                if unsafe { libc::kill(pid, signal) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error.into());
                    }
                }
            }
            Ok(())
        }
        fn alive(&self) -> anyhow::Result<bool> {
            // Cleanup discards the active child's exit status intentionally. No
            // subsequent process is spawned until all adopted children are reaped.
            for group in &self.groups {
                while unsafe { libc::waitpid(-group, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
            }
            let children = self.adopted()?;
            for pid in &children {
                unsafe {
                    libc::waitpid(*pid, std::ptr::null_mut(), libc::WNOHANG);
                }
            }
            Ok(self
                .groups
                .iter()
                .any(|g| unsafe { libc::kill(-g, 0) } == 0)
                || !self.adopted()?.is_empty())
        }
        pub fn cleanup(
            &mut self,
            grace: Duration,
            _code: i32,
            _normal: bool,
        ) -> anyhow::Result<()> {
            let mut sent = HashSet::new();
            self.send(self.signal().unwrap_or(libc::SIGTERM), &mut sent)?;
            let start = Instant::now();
            while self.alive()? && start.elapsed() < grace && self.count.load(Ordering::SeqCst) < 2
            {
                // A cancellation arriving during normal-exit cleanup must be forwarded too.
                self.send(self.signal().unwrap_or(libc::SIGTERM), &mut sent)?;
                self.wait(Duration::from_millis(20));
            }
            self.send(libc::SIGKILL, &mut sent)?;
            // Reparenting can happen after KILL. Discover adopted children repeatedly.
            let start = Instant::now();
            while self.alive()? && start.elapsed() < Duration::from_secs(2) {
                self.send(libc::SIGKILL, &mut sent)?;
                self.wait(Duration::from_millis(10));
            }
            #[cfg(target_os = "linux")]
            if self.alive()? {
                anyhow::bail!("owned processes did not exit within 2s after KILL");
            }
            self.groups.clear();
            Ok(())
        }
    }
    impl Drop for Processes {
        fn drop(&mut self) {
            for handler in self.handlers.drain(..) {
                signal_hook::low_level::unregister(handler);
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};
    use windows_sys::Win32::{
        Foundation::*,
        System::{Console::*, JobObjects::*, Threading::*},
    };
    static SIGNAL: AtomicI32 = AtomicI32::new(0);
    static WAKE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    unsafe extern "system" fn handler(kind: u32) -> i32 {
        let sig = match kind {
            CTRL_C_EVENT => 2,
            CTRL_BREAK_EVENT => 15,
            _ => return 0,
        };
        let _ = SIGNAL.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        unsafe {
            SetEvent(WAKE.load(Ordering::SeqCst));
        }
        1
    }
    pub struct Processes {
        job: HANDLE,
        wake: HANDLE,
    }
    impl Processes {
        pub fn new() -> anyhow::Result<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error().into());
                }
                let wake = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
                if wake.is_null() {
                    CloseHandle(job);
                    return Err(std::io::Error::last_os_error().into());
                }
                let result = Self { job, wake };
                WAKE.store(wake, Ordering::SeqCst);
                if AssignProcessToJobObject(job, GetCurrentProcess()) == 0
                    || SetConsoleCtrlHandler(Some(handler), 1) == 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                Ok(result)
            }
        }
        pub fn signal(&self) -> Option<i32> {
            match SIGNAL.load(Ordering::SeqCst) {
                0 => None,
                s => Some(s),
            }
        }
        pub fn prepare(&self, cmd: &mut Command) {
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        pub fn track(&mut self, _: u32) {}
        pub fn prune_groups(&mut self) {}
        pub fn wait(&mut self, duration: Duration) {
            unsafe {
                WaitForSingleObject(self.wake, duration.as_millis() as u32);
            }
        }
        pub fn cleanup(&mut self, _: Duration, _code: i32, _normal: bool) -> anyhow::Result<()> {
            unsafe {
                // Never terminate our own job: the runner must report the
                // observed outcome after either normal exit or a stop.
                let start = Instant::now();
                let mut buffer = vec![0usize; 128];
                loop {
                    if start.elapsed() > Duration::from_secs(2) {
                        anyhow::bail!("job descendants did not exit after termination");
                    }
                    if QueryInformationJobObject(
                        self.job,
                        JobObjectBasicProcessIdList,
                        buffer.as_mut_ptr().cast(),
                        (buffer.len() * size_of::<usize>()) as u32,
                        std::ptr::null_mut(),
                    ) == 0
                    {
                        if GetLastError() == ERROR_MORE_DATA {
                            buffer.resize(buffer.len() * 2, 0);
                            continue;
                        }
                        return Err(std::io::Error::last_os_error().into());
                    }
                    let list = &*(buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>());
                    let pids = std::slice::from_raw_parts(
                        list.ProcessIdList.as_ptr(),
                        list.NumberOfProcessIdsInList as usize,
                    );
                    if pids.iter().all(|pid| *pid as u32 == GetCurrentProcessId()) {
                        break;
                    }
                    for pid in pids {
                        if start.elapsed() > Duration::from_secs(2) {
                            anyhow::bail!("job descendants did not exit after termination");
                        }
                        if *pid as u32 != GetCurrentProcessId() {
                            let process = OpenProcess(
                                PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                                0,
                                *pid as u32,
                            );
                            if !process.is_null() {
                                let terminated = TerminateProcess(process, 1);
                                let waited = WaitForSingleObject(process, 100);
                                CloseHandle(process);
                                if terminated == 0 && waited != WAIT_OBJECT_0 {
                                    return Err(std::io::Error::last_os_error().into());
                                }
                            } else if GetLastError() != ERROR_INVALID_PARAMETER {
                                return Err(std::io::Error::last_os_error().into());
                            }
                        }
                    }
                    if start.elapsed() > Duration::from_secs(2) {
                        anyhow::bail!("job descendants did not exit after termination");
                    }
                }
            }
            Ok(())
        }
    }
    impl Drop for Processes {
        fn drop(&mut self) {
            unsafe {
                SetConsoleCtrlHandler(Some(handler), 0);
                WAKE.store(std::ptr::null_mut(), Ordering::SeqCst);
                CloseHandle(self.wake);
                CloseHandle(self.job);
            }
        }
    }
}
