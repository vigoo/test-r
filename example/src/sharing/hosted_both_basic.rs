//! Example: `#[test_dep(scope = Hosted, worker = both(Trait))]`.
//!
//! `worker = both(Trait)` lowers one `#[test_dep]` registration into TWO
//! `RegisteredDependency` entries that share a single parent-side owner via
//! [`test_r::core::HostedBothShared`]:
//!
//! - the **descriptor view** ([`HostedDep`]) gives workers a
//!   reconstructed `&LiveServiceOwner` they can use for bulk-data
//!   round-trips (here: a tiny TCP echo loop, standing in for
//!   bulk-data gRPC or similar);
//! - the **RPC view** ([`HostedRpcDep`]) gives workers a
//!   `&LiveControlStub` whose method calls round-trip back to the
//!   parent's owner over the runtime's HostedRpc IPC transport (here:
//!   a tiny monotonic id allocator, standing in for a small control
//!   surface that needs strict cross-worker ordering).
//!
//! Both views resolve to the **same** parent-side owner — the macro's
//! shared `Arc<HostedBothShared>` cache guarantees that the
//! `descriptor()` bytes were derived from the very `LiveServiceOwner`
//! that the HostedRpc dispatcher is calling. That property is what
//! makes the `EnvBasedTestDependencies` shape (descriptor-based bulk
//! data + RPC control surface) safe to express as a single dep.

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread::JoinHandle;
    use test_r::core::{HostedDep, HostedRpcChannel, HostedRpcDep};
    use test_r::{hosted_rpc, test, test_dep};

    /// Counts how many times the owner constructor ran in this process.
    /// Mirrors the singleton-property regression test in
    /// [`super::hosted_basic`] and [`super::hosted_rpc_basic`]: the
    /// `worker = both(...)` lowering must run the owner exactly once
    /// in the top-level parent process, *not* once per registered
    /// view.
    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// Worker-visible RPC trait. The `#[hosted_rpc]` attribute keeps
    /// the trait declaration as-is and adds a generated
    /// `LiveControlStub` struct + `LiveControlDispatch` helper next to
    /// it. The trait carries the *control* surface only; bulk data
    /// goes through the descriptor view (the TCP echo loop).
    #[hosted_rpc]
    pub trait LiveControl {
        /// Allocate the next monotonically-increasing id from the
        /// parent-held counter. Used by the RPC-view regression tests
        /// below to assert that every worker hits the same singleton.
        fn next_id(&self) -> u64;
    }

    /// Owner type. Lives in the parent process for the suite's
    /// duration. Holds **both**:
    ///
    /// - the descriptor-side TCP listener + accept loop (drained
    ///   directly from workers via `LiveServiceOwner::round_trip`),
    ///   and
    /// - the RPC-side monotonic id counter (drained from workers via
    ///   the `LiveControlStub` route).
    pub struct LiveServiceOwner {
        addr: SocketAddr,
        counter: Mutex<u64>,
        // Owner-only state. Workers will never populate these — the
        // descriptor-reconstructed `LiveServiceOwner` on the worker
        // only carries `addr` (and a zeroed counter that workers must
        // never touch directly; the canonical counter is the
        // parent's).
        _listener: Option<Arc<TcpListener>>,
        _accept_thread: Option<Arc<OnceLock<JoinHandle<()>>>>,
    }

    impl LiveServiceOwner {
        fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind localhost");
            let addr = listener.local_addr().expect("local_addr");
            let listener = Arc::new(listener);
            let accept_lock: Arc<OnceLock<JoinHandle<()>>> = Arc::new(OnceLock::new());

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
            accept_lock.set(handle).ok();

            Self {
                addr,
                counter: Mutex::new(0),
                _listener: Some(listener),
                _accept_thread: Some(accept_lock),
            }
        }

        /// Descriptor-side method: round-trips bytes through the
        /// owner's TCP echo loop. Workers reach the parent listener
        /// over the reconstructed `addr`.
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

    /// Descriptor view: workers reconstruct a `LiveServiceOwner`
    /// handle from the parent-shipped `addr` bytes. Same shape a
    /// plain `scope = Hosted` dep would produce.
    impl HostedDep for LiveServiceOwner {
        fn descriptor(&self) -> Vec<u8> {
            self.addr.to_string().into_bytes()
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
            let s = std::str::from_utf8(bytes).expect("utf-8 descriptor");
            let addr: SocketAddr = s.parse().expect("parse socket addr");
            Self {
                addr,
                // Workers must never produce ids locally — the
                // parent's counter is the singleton. Initialize to
                // zero so any accidental local read is obviously
                // wrong.
                counter: Mutex::new(0),
                _listener: None,
                _accept_thread: None,
            }
        }
    }

    /// `LiveControl` impl. The owner is what answers `next_id()`
    /// from the parent side; the `#[hosted_rpc]`-generated dispatch
    /// helper routes incoming RPCs to it. The body matches the
    /// canonical counter at `self.counter`.
    impl LiveControl for LiveServiceOwner {
        fn next_id(&self) -> u64 {
            let mut g = self.counter.lock().unwrap();
            *g += 1;
            *g
        }
    }

    /// RPC view: one-line `HostedRpcDep` impl thanks to the
    /// `#[hosted_rpc]`-generated `LiveControlStub` /
    /// `LiveControlDispatch`. The macro guarantees the method-index
    /// table on both sides matches.
    impl HostedRpcDep for LiveServiceOwner {
        type Stub = LiveControlStub;

        fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            LiveControlDispatch::dispatch_live_control(self, method_idx, args)
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            LiveControlStub::new(channel)
        }
    }

    /// The whole point of this file: ONE `#[test_dep]` registration
    /// with `worker = both(LiveControl)` produces both views
    /// (descriptor-reconstructed `&LiveServiceOwner` and
    /// RPC-routed `&LiveControlStub`) backed by the same parent
    /// owner.
    #[test_dep(scope = Hosted, worker = both(LiveControl))]
    fn live_service() -> LiveServiceOwner {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        LiveServiceOwner::new()
    }

    // ----------------------- descriptor-view tests -----------------------

    #[test]
    fn both_descriptor_round_trip_one(service: &LiveServiceOwner) {
        let echoed = service.round_trip(b"both-hello-1");
        assert_eq!(&echoed, b"both-hello-1");
    }

    #[test]
    fn both_descriptor_round_trip_two(service: &LiveServiceOwner) {
        let echoed = service.round_trip(b"both-hello-2");
        assert_eq!(&echoed, b"both-hello-2");
    }

    // -------------------------- RPC-view tests ---------------------------

    #[test]
    fn both_rpc_next_id_returns_positive(ctrl: &LiveControlStub) {
        let id = ctrl.next_id();
        assert!(id > 0, "ids must be positive, got {id}");
    }

    #[test]
    fn both_rpc_next_id_is_monotonic_within_a_test(ctrl: &LiveControlStub) {
        let a = ctrl.next_id();
        let b = ctrl.next_id();
        let c = ctrl.next_id();
        assert!(a < b && b < c, "ids must increase: {a}, {b}, {c}");
    }

    // --------------------- combined-view regression ----------------------

    /// Pins the key Step 4 property: both views in the same test
    /// reach the same parent-side owner. The descriptor view does a
    /// TCP echo round-trip; the RPC view bumps the counter; both
    /// must observe the singleton parent owner created exactly once.
    #[test]
    fn both_views_share_the_same_parent_owner(service: &LiveServiceOwner, ctrl: &LiveControlStub) {
        let echoed = service.round_trip(b"both-shared");
        assert_eq!(&echoed, b"both-shared");

        let id = ctrl.next_id();
        assert!(
            id > 0,
            "RPC view must reach the parent counter, got id {id}"
        );
    }

    /// Mirrors the singleton-property regression test in
    /// [`super::hosted_basic`] / [`super::hosted_rpc_basic`]: the
    /// owner constructor must run exactly once in the top-level
    /// parent and never in an IPC worker subprocess, even though
    /// the `both` lowering emits two `RegisteredDependency` entries
    /// that share the owner.
    #[test]
    fn both_owner_runs_only_in_top_level_parent(
        _service: &LiveServiceOwner,
        _ctrl: &LiveControlStub,
    ) {
        let is_ipc_worker = std::env::args().any(|a| a == "--ipc");
        let runs = OWNER_CTOR_RUNS.load(Ordering::SeqCst);
        if is_ipc_worker {
            assert_eq!(
                runs, 0,
                "`worker = both` owner constructor must NOT run inside an IPC \
                 worker subprocess. Counter value {runs} means the worker \
                 duplicated the owner."
            );
        } else {
            assert_eq!(
                runs, 1,
                "Top-level parent must construct the `worker = both` owner \
                 exactly once (the shared cache must keep both registrations \
                 pointing at the same instance); observed {runs} runs instead."
            );
        }
    }
}
