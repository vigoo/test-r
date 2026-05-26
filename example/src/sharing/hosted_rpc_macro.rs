//! Example: the `#[hosted_rpc]` attribute macro, registered via the
//! `worker = rpc(Trait)` picker.
//!
//! Compare with [`hosted_rpc_basic`](super::hosted_rpc_basic), which writes
//! the worker-side stub, the method-index match arms in
//! [`HostedRpcDep::dispatch`] and the args/result `desert_rust`
//! encode/decode plumbing **by hand**.
//!
//! Here we ask `#[hosted_rpc]` to generate all of that for us from the user
//! trait. The macro emits two items next to the trait declaration:
//!
//! - a `<Trait>Stub` struct holding a [`HostedRpcChannel`], with `<Trait>`
//!   implemented for it by routing every method through the channel; the
//!   args are encoded as a tuple of the parameter types and the return
//!   value is encoded directly (both using `desert_rust`).
//! - a `<Trait>Dispatch` helper trait, blanket-implemented for every
//!   `T: <Trait>`, that exposes a `dispatch_<snake_case_trait_name>(
//!   method_idx, args)` method-table dispatcher so the owner side's
//!   [`HostedRpcDep::dispatch`] becomes a one-line delegation.
//!
//! Registration uses the **HR3.2.0 `worker = rpc(Trait)` sugar**:
//! `#[test_dep(scope = Hosted, worker = rpc(Counter))]` — equivalent to
//! the legacy `#[test_dep(scope = HostedRpc, stub = CounterStub)]` but
//! drops the `Stub`-name boilerplate and names the *trait* the worker
//! talks through. The runtime internally still goes through the
//! HostedRpc pipeline.
//!
//! Wire-compatibility note: the macro picks method indices from the trait's
//! source order starting at `0`, while [`hosted_rpc_basic`] picks them by
//! hand starting at `1`. Both are valid choices; the only requirement is
//! that the owner side and the stub side agree, which is exactly what the
//! macro guarantees by generating both.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use test_r::core::{HostedRpcChannel, HostedRpcDep};
    use test_r::{hosted_rpc, test, test_dep};

    /// Same singleton-property regression as in [`hosted_rpc_basic`]: the
    /// HostedRpc owner constructor must run exactly once in the top-level
    /// parent and never in an IPC worker subprocess.
    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// User-facing trait. The `#[hosted_rpc]` attribute keeps the trait
    /// declaration as-is and *adds* the generated stub + dispatch glue.
    /// Tests in worker subprocesses parameterise on `&CounterStub` (the
    /// generated stub) and the host's `Counter` impl runs in the parent.
    #[hosted_rpc]
    pub trait Counter {
        /// Allocate the next monotonically-increasing id. The macro turns
        /// this into a HostedRpc call that round-trips to the parent's
        /// `CounterOwner::next` impl.
        fn next(&self) -> u64;

        /// Reserve a contiguous range of `count` ids and return the first
        /// reserved id. Exercises the multi-argument / non-unit-arg leg
        /// of the macro's args-tuple encoding.
        fn reserve(&self, count: u32) -> u64;

        /// Round-trip a freeform string so the macro's args-encode /
        /// reply-decode plumbing is exercised for a non-primitive type
        /// (`String`).
        fn echo(&self, msg: String) -> String;

        /// Two-argument method. The wire shape for `>= 2` args goes
        /// through a regular `(T1, T2)` tuple (the 1-arg bare-`T`
        /// special case does NOT apply here), so this method pins the
        /// multi-arg leg of the macro's args-encoding plumbing.
        fn add(&self, a: u32, b: u32) -> u64;

        /// Unit-return method. Pins the `ReturnType::Default` /
        /// "synthesise `()`" path: the macro must encode and decode
        /// `()` on both sides for return values too.
        fn ping(&self);
    }

    /// Owner: lives in the parent process for the whole suite. The
    /// `#[hosted_rpc]` macro doesn't pick a single concrete owner type for
    /// the trait; instead it generates a `CounterDispatch` helper that any
    /// `Counter` impl can delegate to. That lets the owner stay an
    /// ordinary local type with normal Rust state.
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
            // intentionally empty — the side effect we care about is
            // that the RPC round-trip completed.
        }
    }

    /// One-line `HostedRpcDep` impl thanks to the macro-generated
    /// `CounterStub` and `CounterDispatch::dispatch_counter`.
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

    // ----------------------------- tests -----------------------------

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
        let _warmup = c.next(); // make the test order-independent
        let first = c.reserve(5);
        let next_after = c.next();
        assert!(
            next_after >= first + 5,
            "reserve(5) should advance the counter by 5; first={first}, next_after={next_after}"
        );
    }

    #[test]
    fn macro_stub_echo_round_trips_string(c: &CounterStub) {
        let s = "hello-from-#[hosted_rpc]".to_string();
        let back = c.echo(s.clone());
        assert_eq!(back, s, "echo must round-trip a String through the macro");
    }

    /// Two-argument method end-to-end. Exercises the multi-arg wire
    /// shape (regular `(T1, T2)` tuple), not the 1-arg bare-`T` special
    /// case. Pins that the macro encodes / decodes / dispatches a
    /// 2-arg call correctly through the IPC HostedRpc transport.
    #[test]
    fn macro_stub_add_dispatches_two_args(c: &CounterStub) {
        let sum = c.add(7, 35);
        assert_eq!(
            sum, 42,
            "2-arg method must round-trip both args (7 + 35 = 42), got {sum}"
        );
    }

    /// Unit-return method end-to-end. The macro must encode and decode
    /// `()` for `ReturnType::Default` methods (both 0-arg and 0-return
    /// at the same time). After the round-trip the stub must still be
    /// usable for ordinary calls — pins that the IPC framing didn't
    /// desync on the empty reply body.
    #[test]
    fn macro_stub_ping_returns_unit(c: &CounterStub) {
        // The compiler accepting this call already pins the type-level
        // shape (`fn(&Self)` returning `()`). The runtime side asserts
        // the framing stayed in sync by issuing a follow-up call that
        // returns a non-trivial value: if `ping`'s empty `()` reply
        // body had desynced the IPC framing, the next `next()` would
        // hang or decode a bogus value.
        c.ping();
        let id = c.next();
        assert!(id > 0, "post-ping id must be positive, got {id}");
    }

    /// Mirrors the singleton-property regression in [`hosted_rpc_basic`]:
    /// the owner constructor must NEVER run inside an IPC worker
    /// subprocess and must run exactly once in the parent.
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
