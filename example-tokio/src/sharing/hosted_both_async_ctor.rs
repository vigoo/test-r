//! Regression fixture: `#[test_dep(scope = Hosted, worker = both(Trait))]`
//! with an **async** owner constructor (tokio runner).
//!
//! This pins support for async owner constructors on `worker = both(...)`.
//! The golem-side migration of `EnvBasedTestDependencies` (whose `::new(...)`
//! is `async fn`) to `worker = both(RedisControl)` depends on this shape.
//!
//! This file exercises that the macro now accepts an `async fn`
//! constructor for `worker = both(Trait)` and the runtime resolves
//! both worker-side views against a single, asynchronously-constructed
//! owner instance.
//!
//! Companion to [`hosted_both_basic`](super::hosted_both_basic) (sync
//! constructor) and [`hosted_both_async_descriptor`](super::hosted_both_async_descriptor)
//! (sync constructor + async descriptor reconstruction).

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use test_r::core::{HostedDep, HostedRpcChannel, HostedRpcDep};
    use test_r::{hosted_rpc, test, test_dep};

    /// Tracks how many times the owner constructor body actually ran
    /// in this process. The `both(Trait)` lowering must call it at
    /// most once, regardless of how many worker-side views resolve it.
    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// Tiny RPC control surface that proves the `worker = both`
    /// HostedRpc view is correctly wired through to the owner that the
    /// async constructor produced.
    #[hosted_rpc]
    pub trait AsyncBuiltControl {
        fn next_id(&self) -> u64;
        fn build_marker(&self) -> String;
    }

    /// Owner type. Holds a marker string set inside the **async**
    /// constructor body, plus a per-owner counter so the RPC stub can
    /// prove it routes back to the parent.
    pub struct AsyncBuiltOwner {
        marker: String,
        counter: Mutex<u64>,
    }

    impl AsyncBuiltOwner {
        /// Async constructor that does some trivial `.await` work so
        /// the test really exercises the async acquire helper.
        async fn new_async() -> Self {
            // `tokio::task::yield_now` is the simplest call that
            // forces this future to suspend at least once. If the
            // macro accidentally fell back to a sync wrapper this
            // would still compile but would no longer prove anything;
            // the `OWNER_CTOR_RUNS` invariants below catch that case.
            tokio::task::yield_now().await;
            OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
            Self {
                marker: "built-via-async-ctor".to_string(),
                counter: Mutex::new(0),
            }
        }
    }

    impl HostedDep for AsyncBuiltOwner {
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

    impl AsyncBuiltControl for AsyncBuiltOwner {
        fn next_id(&self) -> u64 {
            let mut g = self.counter.lock().unwrap();
            *g += 1;
            *g
        }

        fn build_marker(&self) -> String {
            self.marker.clone()
        }
    }

    impl HostedRpcDep for AsyncBuiltOwner {
        type Stub = AsyncBuiltControlStub;

        fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            AsyncBuiltControlDispatch::dispatch_async_built_control(self, method_idx, args)
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            AsyncBuiltControlStub::new(channel)
        }
    }

    // The key bit this file exists to test: `async fn` constructor +
    // `worker = both(Trait)`.
    #[test_dep(scope = Hosted, worker = both(AsyncBuiltControl))]
    pub async fn async_built_owner() -> AsyncBuiltOwner {
        AsyncBuiltOwner::new_async().await
    }

    // --------------------- descriptor-view tests -----------------------

    #[test]
    async fn async_ctor_descriptor_view_sees_marker_from_async_ctor(owner: &AsyncBuiltOwner) {
        // Workers reconstruct via `from_descriptor`, so they see
        // exactly the marker bytes the async ctor populated on the
        // parent's owner.
        assert_eq!(
            owner.marker, "built-via-async-ctor",
            "descriptor view must surface the marker produced by the parent's \
             async constructor (got {:?})",
            owner.marker
        );
    }

    // ------------------------ RPC-view tests ---------------------------

    #[test]
    async fn async_ctor_rpc_view_routes_back_to_parent(ctrl: &AsyncBuiltControlStub) {
        // The owner-side `build_marker` returns the exact string the
        // async ctor set. If the RPC route somehow short-circuited
        // through a fresh owner, this would either return a different
        // marker or panic, so the assertion is a real cross-view
        // regression.
        let marker = ctrl.build_marker();
        assert_eq!(
            marker, "built-via-async-ctor",
            "RPC view must reach the parent-side owner produced by the \
             async constructor (got {marker:?})",
        );
    }

    #[test]
    async fn async_ctor_rpc_view_monotonic_ids(ctrl: &AsyncBuiltControlStub) {
        let a = ctrl.next_id();
        let b = ctrl.next_id();
        let c = ctrl.next_id();
        assert!(a < b && b < c, "ids must increase: {a}, {b}, {c}");
    }

    // -------------------- combined-view regression ---------------------

    #[test]
    async fn async_ctor_both_views_share_single_parent_owner(
        owner: &AsyncBuiltOwner,
        ctrl: &AsyncBuiltControlStub,
    ) {
        // Descriptor view: the worker-side reconstructed handle sees
        // exactly the marker the async ctor produced on the parent.
        assert_eq!(owner.marker, "built-via-async-ctor");

        // RPC view: routes back to the same parent-held owner that
        // produced the marker above. The shared `HostedBothShared`
        // cell guarantees one owner instance is used for both views.
        let id = ctrl.next_id();
        assert!(
            id > 0,
            "RPC view must reach the parent counter, got id {id}"
        );
        assert_eq!(ctrl.build_marker(), "built-via-async-ctor");
    }

    #[test]
    async fn async_ctor_runs_only_in_top_level_parent(
        _owner: &AsyncBuiltOwner,
        _ctrl: &AsyncBuiltControlStub,
    ) {
        // Mirrors `hosted_both_basic::both_owner_runs_only_in_top_level_parent`
        // but for the async ctor path: the constructor body must run
        // exactly once in the parent (and never inside an IPC worker
        // subprocess). This is the load-bearing guarantee that proves
        // the shared `HostedBothShared` cache de-duplicates across the
        // Hosted-view and HostedRpc-view registrations.
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
                 owner exactly once (the shared cache must keep both \
                 registrations pointing at the same instance); observed \
                 {runs} runs instead."
            );
        }
    }
}
