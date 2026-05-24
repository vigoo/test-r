//! Example: a `Hosted` `#[test_dep]` whose worker-side reconstruction is
//! asynchronous. Compare with [`hosted_basic`](./hosted_basic.rs) (sync
//! `HostedDep`); the only differences here are:
//!
//! * the dep implements [`test_r::core::AsyncHostedDep`] instead of
//!   [`test_r::core::HostedDep`];
//! * `from_descriptor` is `async fn` and can `.await` (e.g. open async
//!   network clients).
//!
//! No `async_worker` flag is required: under the `tokio` runtime,
//! `scope = Hosted` now auto-selects the async worker reconstruction
//! path.
//!
//! Use this whenever worker reconstruction must call async constructors —
//! the wider golem rollout uses it to attach to `Provided*::new(...).await`
//! handles inside `EnvBasedTestDependencies::from_descriptor`.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::time::Duration;
    use test_r::core::AsyncHostedDep;
    use test_r::{test, test_dep};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);
    static WORKER_RECONSTRUCT_RUNS: AtomicUsize = AtomicUsize::new(0);

    pub struct LiveAsyncService {
        addr: SocketAddr,
        /// Pre-opened tokio TCP client. The worker reconstruction opens
        /// this asynchronously in `AsyncHostedDep::from_descriptor`,
        /// which is the whole point of having an async `from_descriptor`:
        /// a sync `HostedDep::from_descriptor` couldn't `.await` here.
        prewarmed_client: Option<tokio::sync::Mutex<TcpStream>>,
        // Owner-only state. Workers don't populate this.
        _accept_task: Option<Arc<OnceLock<JoinHandle<()>>>>,
    }

    impl LiveAsyncService {
        async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("local_addr");
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
                prewarmed_client: None,
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

        /// Demonstrates that the worker-reconstructed handle was indeed
        /// async-warmed: it carries a pre-opened tokio TCP stream that
        /// the worker side reuses without paying another connect cost.
        pub async fn round_trip_prewarmed(&self, payload: &[u8]) -> Vec<u8> {
            let stream = self
                .prewarmed_client
                .as_ref()
                .expect("worker-side prewarmed client must be populated");
            let mut guard = stream.lock().await;
            guard.write_all(payload).await.expect("write");
            let mut buf = vec![0u8; payload.len()];
            tokio::time::timeout(Duration::from_secs(5), guard.read_exact(&mut buf))
                .await
                .expect("echo read timed out")
                .expect("echo read");
            buf
        }
    }

    impl AsyncHostedDep for LiveAsyncService {
        fn descriptor(&self) -> Vec<u8> {
            self.addr.to_string().into_bytes()
        }

        async fn from_descriptor(bytes: &[u8]) -> Self {
            WORKER_RECONSTRUCT_RUNS.fetch_add(1, Ordering::SeqCst);
            let s = std::str::from_utf8(bytes).expect("utf-8 descriptor");
            let addr: SocketAddr = s.parse().expect("parse socket addr");
            // The point of implementing `AsyncHostedDep`: this `.await`
            // is only legal because `from_descriptor` is async there.
            let stream = TcpStream::connect(addr).await.expect("connect");
            Self {
                addr,
                prewarmed_client: Some(tokio::sync::Mutex::new(stream)),
                _accept_task: None,
            }
        }
    }

    /// Async Hosted owner constructor. Same shape as `hosted_basic`;
    /// the difference is purely on the worker side: this owner
    /// implements `AsyncHostedDep` directly so worker reconstruction
    /// is asynchronous. Under the tokio runtime, descriptor-based
    /// Hosted deps now auto-select the async reconstruction path; no
    /// `async_worker` flag is required (it has been deprecated and is
    /// ignored).
    #[test_dep(scope = Hosted)]
    async fn live_async_service() -> LiveAsyncService {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        LiveAsyncService::new().await
    }

    #[test]
    async fn async_hosted_round_trip_fresh_connection(service: &LiveAsyncService) {
        let echoed = service.round_trip(b"fresh-1").await;
        assert_eq!(&echoed, b"fresh-1");
    }

    #[test]
    async fn async_hosted_round_trip_prewarmed_connection(service: &LiveAsyncService) {
        // Mode-consistent Hosted semantics: tests always see the
        // worker-side handle produced by `AsyncHostedDep::from_descriptor`,
        // whether the runner ended up in spawned-worker mode or the
        // no-spawn fallback. Either way, `prewarmed_client` is populated.
        let echoed = service.round_trip_prewarmed(b"prewarm-1").await;
        assert_eq!(&echoed, b"prewarm-1");
    }

    /// Regression test: the parent must still construct the owner exactly
    /// once, even with an async worker reconstructor. Mirrors the same
    /// invariant from `hosted_basic`.
    #[test]
    async fn async_hosted_owner_runs_only_in_top_level_parent(_service: &LiveAsyncService) {
        let is_ipc_worker = std::env::args().any(|a| a == "--ipc");
        let runs = OWNER_CTOR_RUNS.load(Ordering::SeqCst);
        if is_ipc_worker {
            assert_eq!(
                runs, 0,
                "Hosted owner constructor must NOT run inside an IPC \
                 worker subprocess. Counter value {runs} means the worker \
                 duplicated the owner."
            );
        } else {
            assert_eq!(
                runs, 1,
                "Top-level parent must have constructed the Hosted owner \
                 exactly once; observed {runs} runs instead."
            );
        }
    }

    /// Regression test: the worker-side async reconstructor must be
    /// exercised in both the spawned-worker path AND the no-spawn
    /// fallback, so tests always observe the same kind of worker-side
    /// handle. This is the test-r mode-consistent Hosted contract.
    #[test]
    async fn async_hosted_worker_reconstructor_runs_in_worker(_service: &LiveAsyncService) {
        let runs = WORKER_RECONSTRUCT_RUNS.load(Ordering::SeqCst);
        assert!(
            runs >= 1,
            "AsyncHostedDep::from_descriptor must have run at least once \
             in this process (spawned-worker or no-spawn fallback); \
             observed 0 runs."
        );
    }
}
