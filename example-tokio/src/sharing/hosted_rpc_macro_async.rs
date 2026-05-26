//! Example: an **async-mode** `#[hosted_rpc]` trait. Auto-detected
//! from the user's `async fn` methods — no `#[hosted_rpc(async)]`
//! flag, no `async_worker` flag, no opt-in needed.
//!
//! What this pins:
//!
//! - `#[hosted_rpc]` accepts a trait whose every method is `async fn`,
//!   generates a worker-side `<Trait>Stub` whose methods preserve the
//!   `async fn` signature, and generates an owner-side
//!   `<Trait>Dispatch` helper whose `dispatch_<snake>` method is itself
//!   `async fn`.
//! - The owner implements [`test_r::core::AsyncHostedRpcDep`] (instead
//!   of the sync [`test_r::core::HostedRpcDep`]) and may freely
//!   `.await` inside its dispatcher — here we wait on
//!   `tokio::time::sleep(...)` so the test fails to compile if the
//!   generated dispatch isn't actually async.
//! - The transparency rule: the registration site uses the same
//!   `#[test_dep(scope = Hosted, worker = rpc(Trait))]` sugar as the
//!   sync example; only the trait-impl pair changes.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use test_r::core::{AsyncHostedRpcDep, HostedRpcChannel};
    use test_r::{hosted_rpc, test, test_dep};
    use tokio::sync::Mutex;

    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// All methods are `async fn` — the macro auto-detects async-mode
    /// and emits an async dispatch helper plus async stub method
    /// signatures.
    #[hosted_rpc]
    pub trait AsyncCounter {
        async fn next(&self) -> u64;
        async fn add(&self, a: u32, b: u32) -> u64;
    }

    /// Parent-held owner. Uses [`tokio::sync::Mutex`] so each `next` /
    /// `add` dispatch must actually `.await` the lock acquisition (a
    /// sync-only owner would have to settle for `std::sync::Mutex` and
    /// could never demonstrate the value of async dispatch).
    pub struct AsyncCounterOwner {
        counter: Mutex<u64>,
    }

    impl AsyncCounterOwner {
        fn new() -> Self {
            Self {
                counter: Mutex::new(0),
            }
        }
    }

    impl AsyncCounter for AsyncCounterOwner {
        async fn next(&self) -> u64 {
            // Real `.await` inside the user method: yields once and
            // pauses ~1 ms so the dispatcher proves it isn't quietly
            // running this synchronously. Any sync-only dispatch path
            // for an async owner would deadlock or panic here.
            tokio::time::sleep(Duration::from_millis(1)).await;
            let mut g = self.counter.lock().await;
            *g += 1;
            *g
        }

        async fn add(&self, a: u32, b: u32) -> u64 {
            tokio::task::yield_now().await;
            a as u64 + b as u64
        }
    }

    /// Owner-side glue. The async-mode `#[hosted_rpc]` macro generates
    /// `async fn dispatch_async_counter(...)`, so the owner implements
    /// [`AsyncHostedRpcDep`] and forwards to it with `.await`. No
    /// explicit `async` flag is needed on either the macro or the
    /// `#[test_dep]` registration; the choice flows from the trait's
    /// `async fn` signature.
    impl AsyncHostedRpcDep for AsyncCounterOwner {
        type Stub = AsyncCounterStub;

        async fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            AsyncCounterDispatch::dispatch_async_counter(self, method_idx, args).await
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            AsyncCounterStub::new(channel)
        }
    }

    /// Same registration sugar as the sync `#[hosted_rpc]` example —
    /// the `worker = rpc(AsyncCounter)` picker is unaware of async-mode.
    #[test_dep(scope = Hosted, worker = rpc(AsyncCounter))]
    fn async_counter_owner() -> AsyncCounterOwner {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        AsyncCounterOwner::new()
    }

    /// Smoke test: the generated stub method is itself `async fn`, so
    /// the test body just `.await`s it. The owner's `next` actually
    /// awaits a `tokio::time::sleep`, so this test only passes if the
    /// dispatch end-to-end runs through the async path.
    #[test]
    async fn async_stub_next_returns_positive_ids(c: &AsyncCounterStub) {
        let id = c.next().await;
        assert!(
            id > 0,
            "async-mode macro-generated next() must be positive, got {id}"
        );
    }

    /// Multi-arg pin: `add(a, b)` on the async stub must round-trip
    /// both args through the async dispatcher. Same wire shape as the
    /// sync example, just with `.await` at the call site.
    #[test]
    async fn async_stub_add_dispatches_two_args(c: &AsyncCounterStub) {
        let sum = c.add(7, 35).await;
        assert_eq!(
            sum, 42,
            "async 2-arg method must round-trip both args (7 + 35 = 42), got {sum}"
        );
    }

    /// Singleton-property regression: the async owner constructor must
    /// run exactly once in the top-level parent, never inside an IPC
    /// worker subprocess.
    #[test]
    async fn async_owner_runs_only_in_top_level_parent(_c: &AsyncCounterStub) {
        let is_ipc_worker = std::env::args().any(|a| a == "--ipc");
        let runs = OWNER_CTOR_RUNS.load(Ordering::SeqCst);
        if is_ipc_worker {
            assert_eq!(
                runs, 0,
                "Async HostedRpc owner constructor must NOT run inside an IPC \
                 worker subprocess; counter value {runs} means the worker \
                 duplicated the owner."
            );
        } else {
            assert_eq!(
                runs, 1,
                "Top-level parent must construct the async HostedRpc owner \
                 exactly once; observed {runs} runs instead."
            );
        }
    }
}
