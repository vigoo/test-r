//! Example: `#[test_dep(scope = Hosted, worker = both(Trait))]`, tokio runner.
//!
//! Mirror of [`hosted_both_basic`](../../example/src/sharing/hosted_both_basic.rs)
//! for the tokio runtime. Demonstrates that the `worker = both(...)`
//! lowering also works on the tokio runner: one
//! `Arc<HostedBothShared>` cell is shared between the
//! Hosted-descriptor registration and the HostedRpc registration so
//! both worker-side views (`&LiveServiceOwner` and
//! `&LiveControlStub`) resolve to the same parent-side owner.
//!
//! This example uses `std::net::TcpListener` from a sync constructor. The
//! listener is converted to a tokio listener inside the owner's `_accept_task`,
//! so the parent's tokio runtime still drives the accept loop.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;
    use test_r::core::{HostedDep, HostedRpcChannel, HostedRpcDep};
    use test_r::{hosted_rpc, test, test_dep};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// Worker-visible RPC trait for the control surface (id allocator).
    #[hosted_rpc]
    pub trait LiveControl {
        fn next_id(&self) -> u64;
    }

    pub struct LiveServiceOwner {
        addr: SocketAddr,
        counter: Mutex<u64>,
        // Owner-only state. Workers don't populate these.
        _accept_task: Option<Arc<OnceLock<JoinHandle<()>>>>,
    }

    impl LiveServiceOwner {
        fn new() -> Self {
            // Bind synchronously and convert to a tokio TcpListener on the
            // parent's runtime; nonblocking is set so the conversion succeeds
            // without rebinding.
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

    impl HostedDep for LiveServiceOwner {
        fn descriptor(&self) -> Vec<u8> {
            self.addr.to_string().into_bytes()
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
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

    // ----------------------- descriptor-view tests -----------------------

    #[test]
    async fn both_descriptor_round_trip_one(service: &LiveServiceOwner) {
        let echoed = service.round_trip(b"both-hello-1").await;
        assert_eq!(&echoed, b"both-hello-1");
    }

    #[test]
    async fn both_descriptor_round_trip_two(service: &LiveServiceOwner) {
        let echoed = service.round_trip(b"both-hello-2").await;
        assert_eq!(&echoed, b"both-hello-2");
    }

    // -------------------------- RPC-view tests ---------------------------

    #[test]
    async fn both_rpc_next_id_returns_positive(ctrl: &LiveControlStub) {
        let id = ctrl.next_id();
        assert!(id > 0, "ids must be positive, got {id}");
    }

    #[test]
    async fn both_rpc_next_id_is_monotonic_within_a_test(ctrl: &LiveControlStub) {
        let a = ctrl.next_id();
        let b = ctrl.next_id();
        let c = ctrl.next_id();
        assert!(a < b && b < c, "ids must increase: {a}, {b}, {c}");
    }

    // --------------------- combined-view regression ----------------------

    #[test]
    async fn both_views_share_the_same_parent_owner(
        service: &LiveServiceOwner,
        ctrl: &LiveControlStub,
    ) {
        let echoed = service.round_trip(b"both-shared").await;
        assert_eq!(&echoed, b"both-shared");

        let id = ctrl.next_id();
        assert!(
            id > 0,
            "RPC view must reach the parent counter, got id {id}"
        );
    }

    #[test]
    async fn both_owner_runs_only_in_top_level_parent(
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
