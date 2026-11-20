//! One invocation clock and one owner for all spawned processes. Signal handlers
//! never inspect children: spawn publishes ownership before the loop checks flags.
use crate::run::{BinaryOutcome, StopReason};
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
    /// Installs signal handling and process containment. Create it only when
    /// processes are about to run: until then the default signal behaviour
    /// keeps loading and verification interruptible.
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

    pub fn stop_reason(&self) -> Option<StopReason> {
        if let Some(signal) = self.platform.signal() {
            Some(StopReason::Signal(signal))
        } else if self.deadline.is_some_and(|d| self.started.elapsed() >= d) {
            Some(StopReason::Deadline)
        } else {
            None
        }
    }

    pub fn stop_if_requested(&mut self) -> Option<StopReason> {
        let reason = self.stop_reason()?;
        self.cleanup();
        self.stop_reason().or(Some(reason))
    }

    fn cleanup(&mut self) {
        if let Err(error) = self.platform.cleanup(self.grace) {
            eprintln!("Process cleanup failed: {error:#}");
            self.cleanup_failed = true;
        }
    }

    pub fn run(
        &mut self,
        command: &mut Command,
        cleanup: bool,
        mut events: Option<&mut crate::events::BinaryEvents<'_>>,
    ) -> BinaryOutcome {
        if let Some(reason) = self.stop_if_requested() {
            if let Some(events) = events.as_deref_mut() {
                events.stop(reason);
            }
            return reason.into();
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
                    events.stop(reason);
                }
                self.cleanup();
                let _ = child.try_wait();
                // A signal during deadline cleanup replaces the reason; log it
                // for this binary too (each kind is logged once).
                let reason = self.stop_reason().unwrap_or(reason);
                if let Some(events) = events.as_deref_mut() {
                    events.stop(reason);
                }
                return reason.into();
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if cleanup {
                        self.cleanup();
                    } else {
                        self.platform.prune_groups();
                    }
                    // Cleanup is invocation work; it must not rewrite a binary
                    // that already exited. The caller checks invocation stop next.
                    return status.into();
                }
                Ok(None) => self.platform.wait(Duration::from_millis(20)),
                Err(error) => {
                    self.cleanup();
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

    /// How long processes may take to disappear after KILL.
    const KILL_DRAIN: Duration = Duration::from_secs(2);

    /// Owns every process the runner starts, through one process group per
    /// binary and, on Linux, the subreaper role for orphans.
    ///
    /// Invariant: exactly one binary runs at a time, only the main thread
    /// spawns processes, and nothing else in this process spawns or waits for
    /// children. Cleanup reaps every child it finds, including the active
    /// binary behind `std::process::Child`'s back, and treats every child
    /// outside a tracked group as an escaped descendant. Running binaries in
    /// parallel needs per-binary ownership instead.
    pub struct Processes {
        groups: Vec<i32>,
        signal: Arc<AtomicI32>,
        count: Arc<AtomicUsize>,
        wake: UnixStream,
        handlers: Vec<signal_hook::SigId>,
    }

    /// Keeps the first error and lets cleanup continue escalating.
    fn note(first: &mut Option<anyhow::Error>, error: impl Into<anyhow::Error>) {
        first.get_or_insert_with(|| error.into());
    }

    impl Processes {
        pub fn new() -> anyhow::Result<Self> {
            #[cfg(target_os = "linux")]
            linux::init()?;
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
        fn adopted(&self, first: &mut Option<anyhow::Error>) -> Vec<i32> {
            #[cfg(target_os = "linux")]
            {
                linux::children().unwrap_or_else(|error| {
                    note(first, error);
                    vec![]
                })
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = first;
                vec![]
            }
        }
        fn send(
            &mut self,
            signal: i32,
            sent: &mut HashSet<(i32, i32)>,
            first: &mut Option<anyhow::Error>,
        ) {
            self.prune_groups();
            let escaped = self.adopted(first).into_iter().filter(|pid| {
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
                        note(first, error);
                    }
                }
            }
        }
        fn alive(&self, first: &mut Option<anyhow::Error>) -> bool {
            // Cleanup discards the active child's exit status intentionally. No
            // subsequent process is spawned until all adopted children are reaped.
            for group in &self.groups {
                while unsafe { libc::waitpid(-group, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
            }
            for pid in self.adopted(first) {
                unsafe {
                    libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
                }
            }
            self.groups
                .iter()
                .any(|g| unsafe { libc::kill(-g, 0) } == 0)
                || !self.adopted(first).is_empty()
        }
        /// Best effort: an error never stops escalation to KILL. Returns the
        /// first error once everything owned is gone or the drain timed out.
        pub fn cleanup(&mut self, grace: Duration) -> anyhow::Result<()> {
            let mut first = None;
            let mut sent = HashSet::new();
            self.send(
                self.signal().unwrap_or(libc::SIGTERM),
                &mut sent,
                &mut first,
            );
            let start = Instant::now();
            while self.alive(&mut first)
                && start.elapsed() < grace
                && self.count.load(Ordering::SeqCst) < 2
            {
                // A cancellation arriving during normal-exit cleanup must be forwarded too.
                self.send(
                    self.signal().unwrap_or(libc::SIGTERM),
                    &mut sent,
                    &mut first,
                );
                self.wait(Duration::from_millis(20));
            }
            self.send(libc::SIGKILL, &mut sent, &mut first);
            // Reparenting can happen after KILL. Discover adopted children repeatedly.
            let start = Instant::now();
            while self.alive(&mut first) && start.elapsed() < KILL_DRAIN {
                self.send(libc::SIGKILL, &mut sent, &mut first);
                self.wait(Duration::from_millis(10));
            }
            if self.alive(&mut first) {
                note(
                    &mut first,
                    anyhow::anyhow!(
                        "owned processes did not exit within {KILL_DRAIN:?} after KILL"
                    ),
                );
            }
            self.groups.clear();
            first.map_or(Ok(()), Err)
        }
    }
    impl Drop for Processes {
        fn drop(&mut self) {
            for handler in self.handlers.drain(..) {
                signal_hook::low_level::unregister(handler);
            }
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use anyhow::Context;

        /// Become the subreaper for orphaned descendants, and require the
        /// `/proc` child lists cleanup uses to find them.
        pub fn init() -> anyhow::Result<()> {
            let path = format!("/proc/self/task/{}/children", std::process::id());
            std::fs::read_to_string(&path).with_context(|| {
                format!("Process cleanup needs {path} (a kernel with CONFIG_PROC_CHILDREN)")
            })?;
            if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }

        /// Direct children of every thread: orphans are adopted by whichever
        /// thread of the subreaper the kernel picks.
        pub fn children() -> anyhow::Result<Vec<i32>> {
            let mut pids = Vec::new();
            for task in std::fs::read_dir("/proc/self/task")? {
                let path = task?.path().join("children");
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        for pid in text.split_whitespace() {
                            pids.push(pid.parse()?);
                        }
                    }
                    // The thread exited after the directory was listed.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| format!("reading {}", path.display()));
                    }
                }
            }
            pids.sort_unstable();
            pids.dedup();
            Ok(pids)
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

    /// How long job members may take to disappear after termination.
    const KILL_DRAIN: Duration = Duration::from_secs(2);

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

    /// The runner and every descendant share one Job Object. Only one
    /// `Processes` exists per process: the console handler state is global.
    /// Nothing is released on drop. Closing the job handle would terminate
    /// the runner itself (it is a member). The console handler and its wake
    /// event stay installed until exit, so a late Ctrl+C neither takes the
    /// default action, losing the exit code, nor signals a closed handle.
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
                    let error = std::io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error.into());
                }
                let result = Self { job, wake };
                WAKE.store(wake, Ordering::SeqCst);
                // When the runner exits, even forcibly, closing its last job
                // handle terminates every remaining member. A daemon that must
                // outlive the run (such as a compiler cache server) can still
                // leave the job by starting with CREATE_BREAKAWAY_FROM_JOB.
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags =
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                    || AssignProcessToJobObject(job, GetCurrentProcess()) == 0
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
        /// Job members other than the runner, which must stay alive to report.
        fn members(&self) -> anyhow::Result<Vec<u32>> {
            let mut buffer = vec![0usize; 128];
            unsafe {
                while QueryInformationJobObject(
                    self.job,
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * size_of::<usize>()) as u32,
                    std::ptr::null_mut(),
                ) == 0
                {
                    if GetLastError() != ERROR_MORE_DATA {
                        return Err(std::io::Error::last_os_error().into());
                    }
                    buffer.resize(buffer.len() * 2, 0);
                }
                // The id list extends past the struct's declared one-element
                // array, so address it from the buffer, not the struct field.
                let list = buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
                let count = (*list).NumberOfProcessIdsInList as usize;
                let ids = buffer
                    .as_ptr()
                    .cast::<u8>()
                    .add(std::mem::offset_of!(
                        JOBOBJECT_BASIC_PROCESS_ID_LIST,
                        ProcessIdList
                    ))
                    .cast::<usize>();
                let me = GetCurrentProcessId();
                Ok(std::slice::from_raw_parts(ids, count)
                    .iter()
                    .map(|pid| *pid as u32)
                    .filter(|pid| *pid != me)
                    .collect())
            }
        }
        /// Terminates every member without a grace period, then waits for all
        /// of them. Best effort: errors are reported after the drain.
        pub fn cleanup(&mut self, _grace: Duration) -> anyhow::Result<()> {
            let start = Instant::now();
            let mut first: Option<anyhow::Error> = None;
            loop {
                let members = match self.members() {
                    Ok(members) => members,
                    Err(error) => return Err(first.unwrap_or(error)),
                };
                if members.is_empty() {
                    return first.map_or(Ok(()), Err);
                }
                if start.elapsed() > KILL_DRAIN {
                    return Err(first.unwrap_or_else(|| {
                        anyhow::anyhow!(
                            "job descendants did not exit within {KILL_DRAIN:?} after termination"
                        )
                    }));
                }
                let mut terminated = Vec::new();
                for pid in members {
                    unsafe {
                        let process = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
                        if process.is_null() {
                            // The process already exited.
                            if GetLastError() != ERROR_INVALID_PARAMETER {
                                first.get_or_insert_with(|| std::io::Error::last_os_error().into());
                            }
                            continue;
                        }
                        let error =
                            (TerminateProcess(process, 1) == 0).then(std::io::Error::last_os_error);
                        terminated.push((process, error));
                    }
                }
                if terminated.is_empty() {
                    // Nothing could be opened this round; avoid spinning.
                    std::thread::sleep(Duration::from_millis(10));
                }
                for (process, error) in terminated {
                    let remaining = KILL_DRAIN.saturating_sub(start.elapsed());
                    unsafe {
                        let waited = WaitForSingleObject(process, remaining.as_millis() as u32);
                        CloseHandle(process);
                        // Termination fails for a process that is already exiting.
                        if let Some(error) = error
                            && waited != WAIT_OBJECT_0
                        {
                            first.get_or_insert_with(|| error.into());
                        }
                    }
                }
            }
        }
    }
}
