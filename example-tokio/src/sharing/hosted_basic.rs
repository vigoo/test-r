//! Example: a `Hosted` `#[test_dep]` exercised by async tests.
//!
//! See [`hosted_basic`](../../example/src/sharing/hosted_basic.rs) for the
//! sync version and a fuller explanation of the Hosted scope. The tokio
//! variant uses an async TCP echo listener whose accept loop runs inside the
//! parent's tokio runtime; workers connect to it from their own async tests.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;
    use test_r::core::HostedDep;
    use test_r::{test, test_dep};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    pub struct LiveService {
        addr: SocketAddr,
        // Owner-only state. Workers don't populate these.
        _accept_task: Option<Arc<OnceLock<JoinHandle<()>>>>,
    }

    impl LiveService {
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
                _accept_task: Some(accept_lock),
            }
        }

        pub async fn round_trip(&self, payload: &[u8]) -> Vec<u8> {
            // The owner accept loop is awaiting on a background tokio task in
            // the parent process. Each worker test opens its own connection.
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

    impl HostedDep for LiveService {
        fn descriptor(&self) -> Vec<u8> {
            self.addr.to_string().into_bytes()
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
            let s = std::str::from_utf8(bytes).expect("utf-8 descriptor");
            let addr: SocketAddr = s.parse().expect("parse socket addr");
            Self {
                addr,
                _accept_task: None,
            }
        }
    }

    /// Async Hosted owner constructor. The parent awaits this future on its
    /// own runtime (via `collect_hosted_descriptor_bytes_async`), captures
    /// the descriptor bytes, and keeps the returned value alive for the
    /// duration of the suite.
    #[test_dep(scope = Hosted)]
    async fn live_service() -> LiveService {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        LiveService::new().await
    }

    #[test]
    async fn hosted_round_trip_one(service: &LiveService) {
        let echoed = service.round_trip(b"hello-1").await;
        assert_eq!(&echoed, b"hello-1");
    }

    #[test]
    async fn hosted_round_trip_two(service: &LiveService) {
        let echoed = service.round_trip(b"hello-2").await;
        assert_eq!(&echoed, b"hello-2");
    }

    #[test]
    async fn hosted_round_trip_three(service: &LiveService) {
        let echoed = service.round_trip(b"hello-3").await;
        assert_eq!(&echoed, b"hello-3");
    }

    /// Regression test for the oracle's blocking finding: IPC worker
    /// subprocesses must NOT run Hosted owner constructors. See the sync
    /// `hosted_basic` example for a detailed explanation; this is the async
    /// mirror.
    #[test]
    async fn hosted_owner_runs_only_in_top_level_parent(_service: &LiveService) {
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
            assert_eq!(
                runs, 1,
                "Top-level parent must have constructed the Hosted owner \
                 exactly once; observed {runs} runs instead."
            );
        }
    }
}
