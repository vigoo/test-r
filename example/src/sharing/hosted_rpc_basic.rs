//! Example: a `HostedRpc` `#[test_dep]` with a **hand-written** stub /
//! dispatcher.
//!
//! With `scope = HostedRpc` the parent runs the owner constructor exactly
//! once, keeps the owner alive for the suite, and routes worker-initiated
//! method calls back to the owner over the runtime's built-in IPC channel
//! (the same socket the harness uses for `RunTest` / `ProvideCloneable` /
//! `ProvideHostedDescriptor`).
//!
//! Tests in the worker subprocesses see a **stub** — a small handle whose
//! methods serialise their arguments, send a `HostedRpcCall` frame, block
//! until the parent dispatches the call against the owner and responds, and
//! finally deserialise the return value. That makes singleton services like
//! "give me a unique id" safe to share across many parallel worker
//! subprocesses without setting up a real network protocol of your own.
//!
//! This file demonstrates the MVP: a monotonically-increasing id allocator
//! owned by the parent. Workers each get a `LastUniqueIdStub` and every
//! `next()` call routes back to the parent so the ids stay globally unique.
//!
//! **Choosing this style vs the macro sugar.** This file shows the
//! low-level shape: a hand-written `LastUniqueIdStub`, manually-picked
//! method indices, and bytes-in / bytes-out args. Real code that has
//! a normal Rust trait surface should prefer the
//! [`#[hosted_rpc]`](super::hosted_rpc_macro) macro together with the
//! `worker = rpc(Trait)` picker — it generates the stub, the
//! per-method `desert_rust` codec, and the dispatch arms for you.
//! This file is intentionally kept on the legacy
//! `scope = HostedRpc, stub = LastUniqueIdStub` form because there
//! is no trait to plug into `worker = rpc(...)`; the example is
//! about the underlying machinery.

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use test_r::core::{HostedDep, HostedRpcChannel, HostedRpcDep, HostedRpcError};
    use test_r::{test, test_dep};

    /// Counts how many times the owner constructor ran in this process.
    /// Used to assert the singleton property exactly the way the Hosted
    /// example does, but for HostedRpc.
    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// Owner: a parent-held monotonic id source. The owner lives in the
    /// top-level parent for the suite's duration and is the *only* source
    /// of new ids — every worker stub routes back to this counter.
    pub struct LastUniqueIdOwner {
        counter: Mutex<u64>,
    }

    impl LastUniqueIdOwner {
        fn new() -> Self {
            Self {
                counter: Mutex::new(0),
            }
        }
    }

    /// HostedRpc method indices. The implementor picks them; the macro
    /// future-proofs us by sending them as `u32` on the wire. We only have
    /// one method in this MVP.
    const METHOD_NEXT: u32 = 1;
    /// HR1.0: large-payload echo. Returns `size` bytes of a deterministic
    /// pattern so the IPC framing's `u32` length prefix can be exercised
    /// end-to-end well above the historical 64 KiB threshold.
    const METHOD_LARGE_ECHO: u32 = 2;

    impl HostedRpcDep for LastUniqueIdOwner {
        type Stub = LastUniqueIdStub;

        fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            match method_idx {
                METHOD_NEXT => {
                    let mut guard = self.counter.lock().map_err(|e| e.to_string())?;
                    *guard += 1;
                    Ok(guard.to_be_bytes().to_vec())
                }
                METHOD_LARGE_ECHO => {
                    let arr: [u8; 4] = args.try_into().map_err(|_| {
                        "METHOD_LARGE_ECHO requires exactly 4 bytes (size)".to_string()
                    })?;
                    let size = u32::from_be_bytes(arr) as usize;
                    let mut out = vec![0u8; size];
                    for (i, b) in out.iter_mut().enumerate() {
                        *b = (i % 251) as u8;
                    }
                    Ok(out)
                }
                other => Err(format!("LastUniqueIdOwner: unknown method_idx {other}")),
            }
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            LastUniqueIdStub { channel }
        }
    }

    /// Worker-visible handle. Tests parameterise on `&LastUniqueIdStub`.
    /// Every method serialises its arguments, posts a HostedRpcCall, blocks
    /// for the owner's reply, and decodes the result.
    pub struct LastUniqueIdStub {
        channel: HostedRpcChannel,
    }

    impl LastUniqueIdStub {
        pub fn dep_id(&self) -> &str {
            self.channel.dep_id()
        }

        pub fn next(&self) -> Result<u64, HostedRpcError> {
            let bytes = self.channel.call(METHOD_NEXT, Vec::new())?;
            let arr: [u8; 8] = bytes
                .as_slice()
                .try_into()
                .map_err(|e| HostedRpcError::Transport(format!("bad reply length: {e}")))?;
            Ok(u64::from_be_bytes(arr))
        }

        /// Intentionally invokes a method index the owner does not know,
        /// so the parent's `LastUniqueIdOwner::dispatch` returns
        /// `Err("…unknown method_idx …")`. The transport layer wraps that
        /// into [`HostedRpcError::Dispatch`] and ships it back to the
        /// worker. Used by the IPC-error-path end-to-end test below.
        pub fn provoke_unknown_method(&self) -> Result<Vec<u8>, HostedRpcError> {
            const UNKNOWN_METHOD_IDX: u32 = 9999;
            self.channel.call(UNKNOWN_METHOD_IDX, Vec::new())
        }

        /// HR1.0: request `size` bytes back from the owner. Used by the
        /// large-payload framing regression test.
        pub fn large_echo(&self, size: u32) -> Result<Vec<u8>, HostedRpcError> {
            self.channel
                .call(METHOD_LARGE_ECHO, size.to_be_bytes().to_vec())
        }
    }

    #[test_dep(scope = HostedRpc, stub = LastUniqueIdStub)]
    fn unique_id_owner() -> LastUniqueIdOwner {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        LastUniqueIdOwner::new()
    }

    pub struct DerivedFromRpcStub {
        first_id: u64,
        parent_stub_dep_id: String,
    }

    impl HostedDep for DerivedFromRpcStub {
        fn descriptor(&self) -> Vec<u8> {
            let mut bytes = self.first_id.to_be_bytes().to_vec();
            bytes.extend_from_slice(self.parent_stub_dep_id.as_bytes());
            bytes
        }

        fn from_descriptor(bytes: &[u8]) -> Self {
            let (id, dep_id) = bytes.split_at(8);
            let arr: [u8; 8] = id.try_into().expect("8-byte id");
            Self {
                first_id: u64::from_be_bytes(arr),
                parent_stub_dep_id: std::str::from_utf8(dep_id)
                    .expect("utf-8 dep id")
                    .to_string(),
            }
        }
    }

    #[test_dep(scope = Hosted)]
    fn derived_from_rpc_stub(ids: &LastUniqueIdStub) -> DerivedFromRpcStub {
        DerivedFromRpcStub {
            first_id: ids.next().expect("derive id from hosted rpc stub"),
            parent_stub_dep_id: ids.dep_id().to_string(),
        }
    }

    #[test]
    fn downstream_hosted_dep_reads_rpc_stub(dep: &DerivedFromRpcStub) {
        assert!(
            dep.first_id > 0,
            "derived dependency must be built from the HostedRpc stub"
        );
    }

    #[test]
    fn downstream_hosted_dep_rpc_stub_keeps_qualified_dep_id(
        dep: &DerivedFromRpcStub,
        ids: &LastUniqueIdStub,
    ) {
        assert_eq!(
            dep.parent_stub_dep_id,
            ids.dep_id(),
            "HostedRpc stubs built while resolving parent-side constructors must carry the same fully-qualified dep id as ordinary injected stubs"
        );
    }

    #[test]
    fn hosted_rpc_ids_are_positive(ids: &LastUniqueIdStub) {
        let id = ids.next().expect("next id");
        assert!(id > 0, "ids must be positive, got {id}");
    }

    #[test]
    fn hosted_rpc_ids_are_monotonic_within_a_test(ids: &LastUniqueIdStub) {
        let a = ids.next().expect("a");
        let b = ids.next().expect("b");
        let c = ids.next().expect("c");
        assert!(a < b && b < c, "ids must increase: {a}, {b}, {c}");
    }

    #[test]
    fn hosted_rpc_batch_of_ids_is_unique(ids: &LastUniqueIdStub) {
        let mut seen: HashSet<u64> = HashSet::new();
        for _ in 0..32 {
            let id = ids.next().expect("next id");
            assert!(seen.insert(id), "duplicate id {id}");
        }
    }

    /// Cross-worker uniqueness: when the harness spawns N worker
    /// subprocesses with `--test-threads N`, each worker calls `next()` and
    /// dumps the resulting id to stdout. Because every worker routes back
    /// to the same parent owner, the ids must still be monotonic across the
    /// suite, not just within a single test.
    #[test]
    fn hosted_rpc_per_worker_ids_are_greater_than_zero(ids: &LastUniqueIdStub) {
        // Tests in workers run concurrently; we don't try to guess the
        // exact id, just that it's a valid one from the shared parent.
        let id = ids.next().expect("next id");
        assert!(id >= 1, "parent-issued id should be positive, got {id}");
    }

    /// End-to-end IPC error-path regression: a worker subprocess calls an
    /// unknown method index, the parent's `dispatch` returns `Err(...)`, the
    /// runtime ships that across the IPC socket as a `HostedRpcReplyBody::Err`,
    /// and the worker-side stub surfaces it as `HostedRpcError::Dispatch`.
    ///
    /// This is the only end-to-end test that exercises the *error* leg of
    /// the IPC HostedRpc round-trip (`HostedRpcCall` → owner `dispatch` →
    /// `HostedRpcReply { body: Err }` → stub returns `Err`). The unit
    /// tests in `test-r-core` cover the in-process transport's error path;
    /// this test makes sure the full IPC pipeline preserves the same
    /// behaviour when the suite runs under capture with multiple worker
    /// subprocesses.
    #[test]
    fn hosted_rpc_unknown_method_surfaces_as_dispatch_error(ids: &LastUniqueIdStub) {
        let err = ids
            .provoke_unknown_method()
            .expect_err("provoke_unknown_method must fail");
        match err {
            HostedRpcError::Dispatch(msg) => {
                assert!(
                    msg.contains("unknown method_idx 9999"),
                    "expected dispatch error to mention the unknown method index, got '{msg}'"
                );
            }
            HostedRpcError::Transport(msg) => {
                panic!(
                    "expected HostedRpcError::Dispatch but the IPC transport produced \
                     HostedRpcError::Transport({msg}); the parent's owner Err(...) reply \
                     must travel back as Dispatch, not Transport"
                );
            }
        }

        // After surfacing the dispatch error the stub must still be usable
        // for ordinary RPC calls — the error path does not desync the IPC
        // framing.
        let id = ids.next().expect("post-error stub must keep working");
        assert!(id > 0, "post-error id must still be positive, got {id}");
    }

    /// HR1.0 regression: round-trip a payload larger than 64 KiB so the
    /// `u32` IPC length prefix is exercised end-to-end across the
    /// HostedRpc transport (request and reply both carry >64 KiB). The
    /// payload is filled with a deterministic `i % 251` pattern so the
    /// test fails on truncation or corruption, not just on length
    /// mismatch.
    #[test]
    fn hosted_rpc_large_payload_round_trip_exceeds_64_kib(ids: &LastUniqueIdStub) {
        const SIZE: u32 = 256 * 1024;
        let payload = ids.large_echo(SIZE).expect("large_echo");
        assert_eq!(
            payload.len(),
            SIZE as usize,
            "framing dropped or truncated bytes"
        );
        for (i, b) in payload.iter().enumerate() {
            assert_eq!(
                *b,
                (i % 251) as u8,
                "framing corrupted byte at index {i}: expected {}, got {b}",
                (i % 251) as u8
            );
        }
        // Post-condition: the big frame did not desync the next RPC.
        let id = ids
            .next()
            .expect("stub must keep working after a large payload round-trip");
        assert!(id > 0, "post-large-payload id must be positive, got {id}");
    }

    /// Regression: two back-to-back RPCs from the same test body must not
    /// desync the IPC framing — the second `next()` after the first must
    /// succeed and must return a distinct id. The worker-side
    /// `IpcHostedRpcTransport` holds the connection mutex for the full
    /// request/response round-trip, so two calls from the same thread run
    /// sequentially; this test pins that the `request_id` framing in
    /// `IpcResponse::HostedRpcCall` / `IpcCommand::HostedRpcReply` keeps the
    /// protocol in sync across the boundary between two consecutive stub calls.
    #[test]
    fn hosted_rpc_two_back_to_back_calls_do_not_desync_protocol(ids: &LastUniqueIdStub) {
        let a = ids.next().expect("a");
        let b = ids.next().expect("b");
        assert!(
            a != b,
            "back-to-back in-flight calls returned the same id ({a} == {b}); \
             the IPC request-id framing did not keep the calls distinct"
        );
        assert!(a > 0 && b > 0, "ids must be positive");
    }

    /// Mirrors the Hosted singleton-property regression test: an IPC worker
    /// subprocess must NEVER run the HostedRpc owner constructor — that's
    /// the whole point of the scope. The top-level parent must run it
    /// exactly once.
    #[test]
    fn hosted_rpc_owner_runs_only_in_top_level_parent(_ids: &LastUniqueIdStub) {
        let is_ipc_worker = std::env::args().any(|a| a == "--ipc");
        let runs = OWNER_CTOR_RUNS.load(Ordering::SeqCst);
        if is_ipc_worker {
            assert_eq!(
                runs, 0,
                "HostedRpc owner constructor must NOT run inside an IPC \
                 worker subprocess (the parent owns the singleton). \
                 Counter value {runs} means the worker duplicated the owner."
            );
        } else {
            assert_eq!(
                runs, 1,
                "Top-level parent must construct the HostedRpc owner \
                 exactly once; observed {runs} runs instead."
            );
        }
    }
}
