//! Example: a `Hosted` `#[test_dep]`.
//!
//! With `scope = Hosted` the parent runs the owner constructor exactly once
//! and **keeps the owner alive** for the duration of the entire test suite.
//! The owner produces a small [`HostedDep::descriptor`] (just bytes), which
//! the parent ships to every worker child. Each worker calls
//! [`HostedDep::from_descriptor`] to reconstruct a per-worker handle that
//! typically connects to the singleton owner held by the parent.
//!
//! This is the right strategy for singleton services that must not be
//! duplicated across worker processes — TCP listeners, Docker containers,
//! env-based test environments, gRPC server clients, and similar.
//!
//! In this example the "owner" is an in-process TCP listener (a stand-in for
//! any singleton service). The descriptor is just the listener's address;
//! each worker reconstructs a `LiveServiceClient` that knows how to connect
//! to it.

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::thread::JoinHandle;
    use test_r::core::HostedDep;
    use test_r::{test, test_dep};

    /// Counts how many times the owner constructor ran in this process.
    /// Used to demonstrate that the owner runs exactly once even when the
    /// parent spawns multiple workers.
    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// Owner: an in-process echo TCP listener. The owner holds the listener
    /// and an accept loop thread; both stay alive as long as the parent
    /// process is running.
    ///
    /// Workers don't see this struct directly — they see the same type but
    /// only ever populate the `addr` field via `from_descriptor`. The
    /// `_listener` and `_accept_thread` fields are owner-only and remain
    /// `None` in the worker handles.
    pub struct LiveService {
        addr: SocketAddr,
        // Owner-only state. Workers will never populate these.
        _listener: Option<Arc<TcpListener>>,
        _accept_thread: Option<Arc<OnceLock<JoinHandle<()>>>>,
    }

    impl LiveService {
        fn new() -> Self {
            // Bind to an ephemeral port so multiple parallel test suites can
            // coexist on the same machine.
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
            let addr = listener.local_addr().expect("local_addr");
            let listener = Arc::new(listener);
            let accept_lock: Arc<OnceLock<JoinHandle<()>>> = Arc::new(OnceLock::new());

            // Tiny echo loop on a background thread. The owner Arc keeps
            // both the listener and the thread handle alive.
            let accept_listener = listener.clone();
            let handle = std::thread::spawn(move || {
                while let Ok((mut stream, _)) = accept_listener.accept() {
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 1024];
                        if let Ok(n) = stream.read(&mut buf) {
                            let _ = stream.write_all(&buf[..n]);
                        }
                    });
                }
            });
            // Lock the handle so the accept loop is owned by the static.
            // Errors here would mean we ran the owner constructor twice;
            // OWNER_CTOR_RUNS would catch that too.
            accept_lock.set(handle).ok();

            Self {
                addr,
                _listener: Some(listener),
                _accept_thread: Some(accept_lock),
            }
        }

        /// What workers actually use: send some bytes through the live owner
        /// listener and verify the echo round-trip works.
        pub fn round_trip(&self, payload: &[u8]) -> Vec<u8> {
            let mut stream = TcpStream::connect(self.addr).expect("connect to owner");
            stream.write_all(payload).expect("write to owner");
            let mut buf = vec![0u8; payload.len()];
            stream
                .read_exact(&mut buf)
                .expect("read echoed bytes from owner");
            buf
        }
    }

    impl HostedDep for LiveService {
        fn descriptor(&self) -> Vec<u8> {
            // The descriptor only needs to carry enough information for the
            // worker handle to reconnect: the address string is plenty.
            self.addr.to_string().into_bytes()
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
            let s = std::str::from_utf8(bytes).expect("utf-8 descriptor");
            let addr: SocketAddr = s.parse().expect("parse socket addr");
            Self {
                addr,
                // Worker handles never own the listener or the accept loop —
                // those live in the parent process.
                _listener: None,
                _accept_thread: None,
            }
        }
    }

    #[test_dep(scope = Hosted)]
    fn live_service() -> LiveService {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        LiveService::new()
    }

    #[test]
    fn hosted_round_trip_one(service: &LiveService) {
        let echoed = service.round_trip(b"hello-1");
        assert_eq!(&echoed, b"hello-1");
    }

    #[test]
    fn hosted_round_trip_two(service: &LiveService) {
        let echoed = service.round_trip(b"hello-2");
        assert_eq!(&echoed, b"hello-2");
    }

    #[test]
    fn hosted_round_trip_three(service: &LiveService) {
        let echoed = service.round_trip(b"hello-3");
        assert_eq!(&echoed, b"hello-3");
    }

    /// Regression test for the oracle's blocking finding: IPC worker
    /// subprocesses must NOT run Hosted owner constructors. The owner
    /// must be materialised in the top-level parent exactly once, even
    /// when the parent spawns N worker children that each re-load this
    /// binary.
    ///
    /// This test runs *inside* whichever process the harness picked
    /// (the parent under `--nocapture`, or a worker subprocess under
    /// capture mode with `--test-threads N`). It looks at the current
    /// process's CLI args to figure out which side it is on, and then
    /// asserts the per-process `OWNER_CTOR_RUNS` counter matches that
    /// side.
    #[test]
    fn hosted_owner_runs_only_in_top_level_parent(_service: &LiveService) {
        // The harness re-executes worker children with `--ipc <name>` in
        // their command line — see `args.rs` for where the parent appends
        // this flag when spawning workers.
        let is_ipc_worker = std::env::args().any(|a| a == "--ipc");
        let runs = OWNER_CTOR_RUNS.load(Ordering::SeqCst);
        if is_ipc_worker {
            assert_eq!(
                runs, 0,
                "Hosted owner constructor must NOT run inside an IPC \
                 worker subprocess (parent ships the descriptor instead). \
                 Counter value {runs} means the worker duplicated the owner."
            );
        } else {
            // Parent path: owner was created exactly once by the runner
            // before any tests started.
            assert_eq!(
                runs, 1,
                "Top-level parent must have constructed the Hosted owner \
                 exactly once; observed {runs} runs instead."
            );
        }
    }
}
