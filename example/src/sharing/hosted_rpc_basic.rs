//! Example: a `HostedRpc` `#[test_dep]`.
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use test_r::core::{HostedRpcChannel, HostedRpcDep, HostedRpcError};
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

    impl HostedRpcDep for LastUniqueIdOwner {
        type Stub = LastUniqueIdStub;

        fn dispatch(&mut self, method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
            match method_idx {
                METHOD_NEXT => {
                    let mut guard = self.counter.lock().map_err(|e| e.to_string())?;
                    *guard += 1;
                    Ok(guard.to_be_bytes().to_vec())
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
    }

    #[test_dep(scope = HostedRpc, stub = LastUniqueIdStub)]
    fn unique_id_owner() -> LastUniqueIdOwner {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        LastUniqueIdOwner::new()
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
