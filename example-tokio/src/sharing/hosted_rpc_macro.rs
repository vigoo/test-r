//! Example: the `#[hosted_rpc]` attribute macro under the tokio runner,
//! registered via the `worker = rpc(Trait)` picker.
//!
//! See [`hosted_rpc_macro`](../../example/src/sharing/hosted_rpc_macro.rs)
//! for the sync version and a fuller walk-through of what the macro
//! generates. This tokio variant pins that the macro-generated stub /
//! dispatcher pair works identically under the tokio test runner — same
//! method-index encoding, same args/reply codec, same singleton
//! semantics for the owner.
//!
//! Registration uses the **HR3.2.0 `worker = rpc(Trait)` sugar**:
//! `#[test_dep(scope = Hosted, worker = rpc(Counter))]` — equivalent
//! to the legacy `#[test_dep(scope = HostedRpc, stub = CounterStub)]`
//! and lowered to the same HostedRpc runtime pipeline.
//!
//! The methods here stay synchronous because the `#[hosted_rpc]` MVP
//! does not support `async fn` trait methods. The async runner support
//! is about the *enclosing* test functions, not the trait methods
//! themselves.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use test_r::core::{HostedRpcChannel, HostedRpcDep};
    use test_r::{hosted_rpc, test, test_dep};

    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    #[hosted_rpc]
    pub trait Counter {
        fn next(&self) -> u64;
        fn reserve(&self, count: u32) -> u64;
        fn echo(&self, msg: String) -> String;
        /// Two-argument method. Pins the multi-arg wire shape
        /// (`(T1, T2)` tuple) under the tokio runner.
        fn add(&self, a: u32, b: u32) -> u64;
        /// Unit-return method under the tokio runner.
        fn ping(&self);
    }

    pub struct CounterOwner {
        counter: Mutex<u64>,
    }

    impl CounterOwner {
        fn new() -> Self {
            Self {
                counter: Mutex::new(0),
            }
        }
    }

    impl Counter for CounterOwner {
        fn next(&self) -> u64 {
            let mut g = self.counter.lock().unwrap();
            *g += 1;
            *g
        }

        fn reserve(&self, count: u32) -> u64 {
            let mut g = self.counter.lock().unwrap();
            let first = *g + 1;
            *g += count as u64;
            first
        }

        fn echo(&self, msg: String) -> String {
            msg
        }

        fn add(&self, a: u32, b: u32) -> u64 {
            a as u64 + b as u64
        }

        fn ping(&self) {
            // intentionally empty — see the matching sync example.
        }
    }

    impl HostedRpcDep for CounterOwner {
        type Stub = CounterStub;

        fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            CounterDispatch::dispatch_counter(self, method_idx, args)
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            CounterStub::new(channel)
        }
    }

    #[test_dep(scope = Hosted, worker = rpc(Counter))]
    fn counter_owner() -> CounterOwner {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        CounterOwner::new()
    }

    #[test]
    fn macro_stub_next_returns_positive_ids(c: &CounterStub) {
        let id = c.next();
        assert!(id > 0, "macro-generated next() must be positive, got {id}");
    }

    #[test]
    fn macro_stub_next_is_monotonic_within_a_test(c: &CounterStub) {
        let a = c.next();
        let b = c.next();
        let d = c.next();
        assert!(a < b && b < d, "ids must increase: {a}, {b}, {d}");
    }

    #[test]
    fn macro_stub_reserve_allocates_a_range(c: &CounterStub) {
        let _warmup = c.next();
        let first = c.reserve(5);
        let next_after = c.next();
        assert!(
            next_after >= first + 5,
            "reserve(5) should advance the counter by 5; first={first}, next_after={next_after}"
        );
    }

    #[test]
    fn macro_stub_echo_round_trips_string(c: &CounterStub) {
        let s = "hello-from-#[hosted_rpc]-tokio".to_string();
        let back = c.echo(s.clone());
        assert_eq!(back, s, "echo must round-trip a String through the macro");
    }

    /// Tokio mirror of the sync 2-arg pinning test. Confirms that the
    /// multi-arg `(T1, T2)` wire shape works end-to-end under the tokio
    /// runner too.
    #[test]
    fn macro_stub_add_dispatches_two_args(c: &CounterStub) {
        let sum = c.add(7, 35);
        assert_eq!(
            sum, 42,
            "2-arg method must round-trip both args (7 + 35 = 42), got {sum}"
        );
    }

    /// Tokio mirror of the sync unit-return pinning test. Confirms
    /// that the empty `()` reply body does not desync the IPC framing
    /// on the tokio transport.
    #[test]
    fn macro_stub_ping_returns_unit(c: &CounterStub) {
        c.ping();
        let id = c.next();
        assert!(id > 0, "post-ping id must be positive, got {id}");
    }

    #[test]
    fn macro_owner_runs_only_in_top_level_parent(_c: &CounterStub) {
        let is_ipc_worker = std::env::args().any(|a| a == "--ipc");
        let runs = OWNER_CTOR_RUNS.load(Ordering::SeqCst);
        if is_ipc_worker {
            assert_eq!(
                runs, 0,
                "HostedRpc owner constructor must NOT run inside an IPC worker subprocess; \
                 counter value {runs} means the worker duplicated the owner."
            );
        } else {
            assert_eq!(
                runs, 1,
                "Top-level parent must construct the HostedRpc owner exactly once; \
                 observed {runs} runs instead."
            );
        }
    }
}
