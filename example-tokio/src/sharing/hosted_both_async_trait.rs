//! Regression fixture: `#[test_dep(scope = Hosted, worker = both(Trait))]`
//! where the `#[hosted_rpc]`-decorated trait itself contains `async fn`
//! methods (tokio runner).
//!
//! This pins the most complex code path in the bug fix:
//!
//! 1. The `#[hosted_rpc]` macro auto-detects async-mode and rewrites
//!    each `async fn` trait method to
//!    `fn(...) -> impl ::core::future::Future<Output = R>
//!        + ::core::marker::Send + '_`.
//!    Without that rewrite the trait's RPITIT futures would not be
//!    statically `Send`, and the shared-owner RPC cell's
//!    `Pin<Box<dyn Future + Send + 'a>>` closure would fail to compile.
//!
//! 2. The macro emits the
//!    `<Trait>Dispatch::dispatch_<snake>_shared_future` helper, which
//!    always returns `Pin<Box<dyn Future + Send + 'a>>` regardless of
//!    sync/async mode.
//!
//! 3. The `worker = both(Trait)` lowering routes through
//!    `HostedRpcOwnerCell::from_shared_owner_async` and dispatches via
//!    the shared `Arc<Owner>` (the same instance the parent-side owner
//!    getter hands back), so the two views share one owner.
//!
//! A downstream `#[test_dep(scope = Hosted)]` consumer that takes
//! `&Owner` exercises the parent-side `HostedBothShared::owner_arc::<T>()`
//! fallback that the original bug report was filed against.
//!
//! Companions to this fixture:
//!
//! - [`hosted_both_basic`](super::hosted_both_basic) — sync owner
//!   constructor, sync `#[hosted_rpc]` trait, exercises the basic
//!   `worker = both` shape.
//! - [`hosted_both_async_ctor`](super::hosted_both_async_ctor) — async
//!   owner constructor, sync `#[hosted_rpc]` trait.
//! - [`hosted_both_async_descriptor`](super::hosted_both_async_descriptor)
//!   — sync constructor, async descriptor reconstruction via
//!   `AsyncHostedDep`.
//! - [`hosted_both_parent_consumer`](super::hosted_both_parent_consumer)
//!   — parent-side downstream `Hosted` consumer, sync `#[hosted_rpc]`
//!   trait.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use test_r::core::{AsyncHostedRpcDep, HostedDep, HostedRpcChannel};
    use test_r::{hosted_rpc, test, test_dep};
    use tokio::sync::Mutex;

    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// Async-mode `#[hosted_rpc]` trait. The macro auto-detects async-mode
    /// because the methods are `async fn`, rewrites each method to
    /// `fn(...) -> impl Future + Send + '_`, and emits the
    /// `dispatch_<snake>_shared_future` helper used by the tokio runtime
    /// when this trait is paired with `worker = both(...)`.
    #[hosted_rpc]
    pub trait AsyncBothControl {
        async fn next_id(&self) -> u64;
        async fn echo_doubled(&self, value: u32) -> u64;
    }

    /// Owner type. Uses a `tokio::sync::Mutex` so each RPC actually
    /// suspends on the async path — any sync-fallback would either
    /// deadlock here or fail the monotonicity assertions below.
    pub struct AsyncBothOwner {
        seed: u32,
        counter: Mutex<u64>,
    }

    impl AsyncBothOwner {
        async fn new() -> Self {
            // Force the constructor to actually .await; if the macro
            // ever fell back to a sync wrapper this would still compile
            // but the per-process singleton invariants pinned below
            // would catch the regression.
            tokio::task::yield_now().await;
            OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
            Self {
                seed: 11,
                counter: Mutex::new(0),
            }
        }
    }

    impl HostedDep for AsyncBothOwner {
        fn descriptor(&self) -> Vec<u8> {
            self.seed.to_le_bytes().to_vec()
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
            let arr: [u8; 4] = bytes.try_into().expect("4-byte seed");
            Self {
                seed: u32::from_le_bytes(arr),
                counter: Mutex::new(0),
            }
        }
    }

    impl AsyncBothControl for AsyncBothOwner {
        async fn next_id(&self) -> u64 {
            // Real `.await` inside the user method: yields once and
            // pauses briefly so the dispatcher proves it isn't quietly
            // running this synchronously.
            tokio::time::sleep(Duration::from_millis(1)).await;
            let mut g = self.counter.lock().await;
            *g += 1;
            *g
        }

        async fn echo_doubled(&self, value: u32) -> u64 {
            tokio::task::yield_now().await;
            (value as u64) * 2
        }
    }

    impl AsyncHostedRpcDep for AsyncBothOwner {
        type Stub = AsyncBothControlStub;

        async fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            AsyncBothControlDispatch::dispatch_async_both_control(self, method_idx, args).await
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            AsyncBothControlStub::new(channel)
        }
    }

    /// The whole point of this file: an async `#[hosted_rpc]` trait
    /// paired with `worker = both(...)` and an async owner constructor.
    #[test_dep(scope = Hosted, worker = both(AsyncBothControl))]
    async fn async_both_owner() -> AsyncBothOwner {
        AsyncBothOwner::new().await
    }

    /// Downstream parent-side `Hosted` consumer. Resolving this on the
    /// parent side reads `&AsyncBothOwner` back from the dep map and
    /// thus exercises `HostedBothShared::owner_arc::<T>()` — the path
    /// the original bug report was filed against — under the async
    /// `#[hosted_rpc]` trait shape.
    pub struct DerivedFromAsync {
        derived_seed: u32,
    }

    impl HostedDep for DerivedFromAsync {
        fn descriptor(&self) -> Vec<u8> {
            self.derived_seed.to_le_bytes().to_vec()
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
            let arr: [u8; 4] = bytes.try_into().expect("4-byte derived seed");
            Self {
                derived_seed: u32::from_le_bytes(arr),
            }
        }
    }

    #[test_dep(scope = Hosted)]
    fn derived(o: &AsyncBothOwner) -> DerivedFromAsync {
        DerivedFromAsync {
            derived_seed: o.seed.wrapping_mul(3) + 2,
        }
    }

    /// End-to-end: the test compiles + runs only if the async dispatch
    /// path through the shared-owner cell is wired up correctly. The
    /// stub method is itself `async fn`, so the body just `.await`s it.
    #[test]
    async fn async_stub_next_id_returns_positive(ctrl: &AsyncBothControlStub) {
        let id = ctrl.next_id().await;
        assert!(
            id > 0,
            "async dispatch through worker = both(AsyncBothControl) must \
             return a positive id from the parent-held owner, got {id}",
        );
    }

    /// Multi-arg round-trip across the async dispatch path; the stub
    /// must serialise `(7)` and decode `42` from the parent's reply.
    #[test]
    async fn async_stub_echo_doubled_round_trips_arg(ctrl: &AsyncBothControlStub) {
        assert_eq!(
            ctrl.echo_doubled(21).await,
            42,
            "async 1-arg method must round-trip the argument through the \
             worker = both(...) shared-owner async dispatch path",
        );
    }

    /// Parent-side downstream `Hosted` consumer reads `&AsyncBothOwner`
    /// out of the dep map. Without the `HostedBothShared::owner_arc::<T>()`
    /// fallback this used to panic with `Dependency type mismatch`.
    #[test]
    async fn downstream_hosted_consumer_reads_owner_async(
        o: &AsyncBothOwner,
        d: &DerivedFromAsync,
    ) {
        assert_eq!(o.seed, 11, "owner seed must round-trip from descriptor");
        assert_eq!(
            d.derived_seed,
            11u32.wrapping_mul(3) + 2,
            "derived seed must equal `owner.seed * 3 + 2`, proving the \
             parent-side owner getter handed back the correct \
             &AsyncBothOwner via HostedBothShared::owner_arc::<T>()",
        );
    }
}
