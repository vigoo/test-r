//! Regression: `worker = both(Trait)` exercised against an **async-only**
//! `AsyncHostedDep` reconstructor under the tokio runtime.
//!
//! Sister fixture to [`hosted_both_basic`](super::hosted_both_basic):
//! that one's owner implements the sync [`HostedDep`] trait, which is
//! also auto-bridged to [`AsyncHostedDep`] via the Step 1 blanket
//! bridge, so its tokio test run is effectively a sync reconstructor
//! tunneled through the async runtime. This fixture instead
//! implements [`AsyncHostedDep`] **directly** with a genuine
//! `async fn from_descriptor(...)` body, so the
//! `worker = both(...)` lowering's tokio
//! [`WorkerReconstructor::Async`] code path is exercised end-to-end:
//! the worker must actually `.await` the user's async reconstruction
//! before injecting `&LiveServiceOwner`, and the RPC view must still
//! resolve to the same parent-side owner through the
//! `HostedBothShared` cache.
//!
//! Oracle HR3.2.0 Step 4 follow-up: this is the missing async-only
//! regression that the sync-backed `hosted_both_basic` example
//! couldn't cover by itself.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;
    use test_r::core::{AsyncHostedDep, HostedRpcChannel, HostedRpcDep};
    use test_r::{hosted_rpc, test, test_dep};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);
    /// Counts how many times an async `from_descriptor` body ran. The
    /// regression asserts this is *non-zero* on workers under capture
    /// mode — proof that the tokio runtime really awaited the async
    /// reconstructor, not a sync-shim.
    static ASYNC_FROM_DESCRIPTOR_AWAITS: AtomicUsize = AtomicUsize::new(0);

    /// RPC trait used as the `worker = both(...)` argument.
    #[hosted_rpc]
    pub trait LiveControl {
        fn next_id(&self) -> u64;
    }

    pub struct LiveServiceOwner {
        addr: SocketAddr,
        counter: Mutex<u64>,
        _accept_task: Option<Arc<OnceLock<JoinHandle<()>>>>,
    }

    impl LiveServiceOwner {
        fn new() -> Self {
            let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind localhost");
            std_listener.set_nonblocking(true).expect("set_nonblocking");
            let addr = std_listener.local_addr().expect("local_addr");
            let listener = TcpListener::from_std(std_listener).expect("tokio listener");

            let accept_lock: Arc<OnceLock<JoinHandle<()>>> = Arc::new(OnceLock::new());

            let handle = tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        let mut buf = [0u8; 1024];
                        if let Ok(n) = stream.read(&mut buf).await {
                            let _ = stream.write_all(&buf[..n]).await;
                        }
                    });
                }
            });
            accept_lock.set(handle).ok();

            Self {
                addr,
                counter: Mutex::new(0),
                _accept_task: Some(accept_lock),
            }
        }

        pub async fn round_trip(&self, payload: &[u8]) -> Vec<u8> {
            let mut stream = TcpStream::connect(self.addr).await.expect("connect");
            stream.write_all(payload).await.expect("write");
            let mut buf = vec![0u8; payload.len()];
            tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut buf))
                .await
                .expect("echo read timed out")
                .expect("echo read");
            buf
        }
    }

    /// Async-only descriptor view: the worker reconstructor MUST
    /// await this body. The `tokio::task::yield_now().await` call is
    /// deliberate — it forces the future to yield at least once so a
    /// hypothetical sync-shim would not satisfy this test.
    impl AsyncHostedDep for LiveServiceOwner {
        fn descriptor(&self) -> Vec<u8> {
            self.addr.to_string().into_bytes()
        }

        async fn from_descriptor(bytes: &[u8]) -> Self {
            ASYNC_FROM_DESCRIPTOR_AWAITS.fetch_add(1, Ordering::SeqCst);
            // Real .await before the rest of the body runs. If
            // anything ever bridged this back to a sync code path the
            // `WorkerReconstructor::Sync` runner would panic at
            // poll time when the closure tries to spawn a tokio task
            // outside a runtime.
            tokio::task::yield_now().await;

            let s = std::str::from_utf8(bytes).expect("utf-8 descriptor");
            let addr: SocketAddr = s.parse().expect("parse socket addr");
            Self {
                addr,
                counter: Mutex::new(0),
                _accept_task: None,
            }
        }
    }

    impl LiveControl for LiveServiceOwner {
        fn next_id(&self) -> u64 {
            let mut g = self.counter.lock().unwrap();
            *g += 1;
            *g
        }
    }

    impl HostedRpcDep for LiveServiceOwner {
        type Stub = LiveControlStub;

        fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            LiveControlDispatch::dispatch_live_control(self, method_idx, args)
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            LiveControlStub::new(channel)
        }
    }

    #[test_dep(scope = Hosted, worker = both(LiveControl))]
    fn live_service() -> LiveServiceOwner {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        LiveServiceOwner::new()
    }

    // -------------------- descriptor view (async) --------------------

    #[test]
    async fn async_descriptor_round_trip_one(service: &LiveServiceOwner) {
        let echoed = service.round_trip(b"async-hello-1").await;
        assert_eq!(&echoed, b"async-hello-1");
    }

    #[test]
    async fn async_descriptor_round_trip_two(service: &LiveServiceOwner) {
        let echoed = service.round_trip(b"async-hello-2").await;
        assert_eq!(&echoed, b"async-hello-2");
    }

    // ------------------------ RPC view (async) -----------------------

    #[test]
    async fn async_rpc_next_id_returns_positive(ctrl: &LiveControlStub) {
        let id = ctrl.next_id();
        assert!(id > 0, "ids must be positive, got {id}");
    }

    // --------------------- combined-view regression -------------------

    #[test]
    async fn async_both_views_share_the_same_parent_owner(
        service: &LiveServiceOwner,
        ctrl: &LiveControlStub,
    ) {
        let echoed = service.round_trip(b"async-both-shared").await;
        assert_eq!(&echoed, b"async-both-shared");

        let id = ctrl.next_id();
        assert!(id > 0, "RPC must reach parent counter, got {id}");
    }

    /// Pins the oracle follow-up: the worker side genuinely awaits
    /// the async `from_descriptor`. Under capture mode the IPC worker
    /// subprocess must have run the async reconstructor at least
    /// once (the dep is injected here), so the await counter is
    /// non-zero. Under `--nocapture` (in-process mode) the parent
    /// does the local reconstruction and the same property holds.
    #[test]
    async fn async_from_descriptor_was_awaited(_service: &LiveServiceOwner) {
        let awaits = ASYNC_FROM_DESCRIPTOR_AWAITS.load(Ordering::SeqCst);
        assert!(
            awaits >= 1,
            "AsyncHostedDep::from_descriptor must have been awaited at least once \
             before this test body ran (got {awaits} awaits). If this is 0 the \
             `worker = both(...)` tokio path is silently using a sync reconstructor."
        );
    }

    /// Mirror of the singleton-property regression in the sync
    /// fixture: the owner constructor must run exactly once in the
    /// top-level parent and never inside an IPC worker subprocess,
    /// even with the async descriptor variant.
    #[test]
    async fn async_owner_runs_only_in_top_level_parent(
        _service: &LiveServiceOwner,
        _ctrl: &LiveControlStub,
    ) {
        let is_ipc_worker = std::env::args().any(|a| a == "--ipc");
        let runs = OWNER_CTOR_RUNS.load(Ordering::SeqCst);
        if is_ipc_worker {
            assert_eq!(
                runs, 0,
                "`worker = both` (async-descriptor) owner constructor must NOT run \
                 inside an IPC worker subprocess. Counter value {runs} means the \
                 worker duplicated the owner."
            );
        } else {
            assert_eq!(
                runs, 1,
                "Top-level parent must construct the `worker = both` (async-descriptor) \
                 owner exactly once; observed {runs} runs instead."
            );
        }
    }
}
