//! Pruning regression: `#[test_dep(scope = Hosted, worker = both(Trait))]`
//! with an **async** owner constructor and tests that parameterise
//! **only** on the `&<Trait>Stub` view.
//!
//! The `worker = both(T)` lowering registers two distinct dep entries
//! — the Hosted owner view (`&Owner`) and the HostedRpc stub view
//! (`&OwnerStub`) — backed by a single shared `Arc<HostedBothShared>`
//! cache. The async-ctor flavour has a sync resolver on the stub side
//! that assumes the Hosted side has already populated the cache. The
//! test-r tokio runner guarantees that ordering, but only when both
//! halves survive the dependency-pruner.
//!
//! The pruner's keep-set normally only follows real constructor edges
//! (`RegisteredDependency.dependencies`). Without an explicit planner
//! link, a suite whose selected tests use **only** the stub view would
//! have the Hosted owner sibling pruned. The async ctor would then
//! never populate the shared cache, and the sync resolver would panic
//! with a `Poll::Pending` diagnostic.
//!
//! The macro now registers the two halves as `companions` of each
//! other, and the pruner expands its keep-set across companions. This
//! fixture pins that contract end-to-end: every test in this module
//! parameterises on `&<Trait>Stub` (and never on `&Owner`), so if the
//! pruner ever stopped retaining companions, this fixture would fail.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use test_r::core::{HostedDep, HostedRpcChannel, HostedRpcDep};
    use test_r::{hosted_rpc, test, test_dep};

    /// Tracks how many times the owner constructor body ran. The
    /// shared cache must keep this at most one in the parent.
    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    #[hosted_rpc]
    pub trait StubOnlyControl {
        fn ping(&self) -> u64;
        fn marker(&self) -> String;
    }

    pub struct StubOnlyOwner {
        marker: String,
        counter: Mutex<u64>,
    }

    impl HostedDep for StubOnlyOwner {
        fn descriptor(&self) -> Vec<u8> {
            self.marker.as_bytes().to_vec()
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
            let marker = std::str::from_utf8(bytes)
                .expect("utf-8 marker descriptor")
                .to_string();
            Self {
                marker,
                counter: Mutex::new(0),
            }
        }
    }

    impl StubOnlyControl for StubOnlyOwner {
        fn ping(&self) -> u64 {
            let mut g = self.counter.lock().unwrap();
            *g += 1;
            *g
        }

        fn marker(&self) -> String {
            self.marker.clone()
        }
    }

    impl HostedRpcDep for StubOnlyOwner {
        type Stub = StubOnlyControlStub;

        fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            StubOnlyControlDispatch::dispatch_stub_only_control(self, method_idx, args)
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            StubOnlyControlStub::new(channel)
        }
    }

    // Async owner constructor + `worker = both(StubOnlyControl)`. The
    // tests below NEVER parameterise on `&StubOnlyOwner`; the Hosted
    // owner sibling must still be retained by the pruner so the
    // shared cache is populated before the sync stub resolver runs.
    #[test_dep(scope = Hosted, worker = both(StubOnlyControl))]
    pub async fn stub_only_owner() -> StubOnlyOwner {
        // Force the future to suspend at least once so this exercises
        // the async acquire helper end-to-end.
        tokio::task::yield_now().await;
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        StubOnlyOwner {
            marker: "stub-only-async".to_string(),
            counter: Mutex::new(0),
        }
    }

    // Every test below uses ONLY the stub view. If the pruner ever
    // stops retaining the Hosted owner companion, the sync resolver
    // in the stub registration would observe an empty
    // `HostedBothShared` cache and panic with the diagnostic message
    // emitted by the macro lowering.

    #[test]
    async fn stub_only_ping_returns_monotonic(ctrl: &StubOnlyControlStub) {
        let a = ctrl.ping();
        let b = ctrl.ping();
        assert!(a < b, "ids must increase: {a}, {b}");
    }

    #[test]
    async fn stub_only_marker_matches_async_ctor(ctrl: &StubOnlyControlStub) {
        let marker = ctrl.marker();
        assert_eq!(
            marker, "stub-only-async",
            "RPC view must reach the parent-side owner produced by the async \
             constructor (got {marker:?})",
        );
    }

    #[test]
    async fn stub_only_ctor_runs_once_in_parent(_ctrl: &StubOnlyControlStub) {
        // Counterpart to `hosted_both_async_ctor::async_ctor_runs_only_in_top_level_parent`,
        // but here NO test in this module references `&StubOnlyOwner` —
        // so if the Hosted owner companion is ever pruned, this test
        // would fail in the parent process *before* we get to the
        // counter assertion (the stub-view sync resolver would panic).
        let is_ipc_worker = std::env::args().any(|a| a == "--ipc");
        let runs = OWNER_CTOR_RUNS.load(Ordering::SeqCst);
        if is_ipc_worker {
            assert_eq!(
                runs, 0,
                "`worker = both` async owner constructor must NOT run inside \
                 an IPC worker subprocess. Counter value {runs} means the \
                 worker duplicated the owner."
            );
        } else {
            assert_eq!(
                runs, 1,
                "Top-level parent must construct the `worker = both` async \
                 owner exactly once via the companion-retained Hosted view, \
                 observed {runs} runs instead."
            );
        }
    }
}
