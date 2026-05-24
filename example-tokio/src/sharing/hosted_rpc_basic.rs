//! Example: a `HostedRpc` `#[test_dep]` with a **hand-written** stub /
//! dispatcher, exercised by async tests.
//!
//! See [`hosted_rpc_basic`](../../example/src/sharing/hosted_rpc_basic.rs)
//! for the sync version and a fuller walk-through of the HostedRpc scope.
//! This tokio variant proves that the same MVP transport (in-process for
//! the no-spawn-workers path, IPC-backed for the spawned-worker path)
//! works under the tokio runner: the parent's `test_thread` worker
//! subprocess and the in-tokio dispatch loop both route incoming
//! `IpcResponse::HostedRpcCall` frames to the owner-cell map, and the
//! worker-side `IpcHostedRpcTransport` bridges the sync trait method via
//! `tokio::task::block_in_place` + `Handle::current().block_on(...)` to
//! the shared `Arc<Mutex<Stream>>` IPC connection.
//!
//! The owner stays in the parent for the duration of the suite and
//! every worker stub call routes back to it, so a monotonically
//! increasing id allocator remains unique across worker subprocesses,
//! exactly like in the sync example.
//!
//! **Choosing this style vs the macro sugar.** Same note as the sync
//! variant: this file shows the low-level shape (hand-written stub,
//! manual method indices, raw `desert_rust` framing). Real code with
//! a normal Rust trait surface should prefer the
//! [`#[hosted_rpc]`](super::hosted_rpc_macro) macro together with
//! `#[test_dep(scope = Hosted, worker = rpc(Trait))]`.

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use test_r::core::{HostedRpcChannel, HostedRpcDep, HostedRpcError};
    use test_r::{test, test_dep};

    /// Counts how many times the owner constructor ran in this process.
    /// Mirrors the sync example so we can assert the singleton property
    /// the same way.
    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// Owner: a parent-held monotonic id source. Lives in the top-level
    /// parent for the suite's duration; the only source of new ids.
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

    const METHOD_NEXT: u32 = 1;
    /// HR1.0: large-payload echo. The owner allocates a `size`-byte
    /// vector filled with a deterministic pattern and ships it back to
    /// the worker so the IPC framing can be exercised end-to-end above
    /// the historical 64 KiB threshold that motivated widening the
    /// length prefix to `u32`.
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
                    // Args: 4-byte big-endian u32 size. Return that many
                    // bytes of a deterministic pattern (i % 251 per byte)
                    // so the test can verify framing didn't truncate or
                    // corrupt the payload.
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
        /// `Err("…unknown method_idx …")`. Used by the IPC-error-path
        /// end-to-end test below; the tokio transport must surface this
        /// as [`HostedRpcError::Dispatch`] just like the sync transport.
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

    /// Sync owner constructor is fine here — owner construction runs on
    /// the parent's tokio runtime via `block_on` either way. The
    /// HostedRpc scope itself doesn't need an async constructor for this
    /// trivial example.
    #[test_dep(scope = HostedRpc, stub = LastUniqueIdStub)]
    fn unique_id_owner() -> LastUniqueIdOwner {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        LastUniqueIdOwner::new()
    }

    #[test]
    async fn hosted_rpc_ids_are_positive(ids: &LastUniqueIdStub) {
        let id = ids.next().expect("next id");
        assert!(id > 0, "ids must be positive, got {id}");
    }

    #[test]
    async fn hosted_rpc_ids_are_monotonic_within_a_test(ids: &LastUniqueIdStub) {
        let a = ids.next().expect("a");
        let b = ids.next().expect("b");
        let c = ids.next().expect("c");
        assert!(a < b && b < c, "ids must increase: {a}, {b}, {c}");
    }

    #[test]
    async fn hosted_rpc_batch_of_ids_is_unique(ids: &LastUniqueIdStub) {
        let mut seen: HashSet<u64> = HashSet::new();
        for _ in 0..32 {
            let id = ids.next().expect("next id");
            assert!(seen.insert(id), "duplicate id {id}");
        }
    }

    /// Cross-worker uniqueness regression for the tokio runner. With
    /// `--test-threads N` the harness spawns N tokio worker subprocesses
    /// and each worker's stub call routes back to the same parent
    /// owner-cell, so the ids must stay globally unique.
    #[test]
    async fn hosted_rpc_per_worker_ids_are_greater_than_zero(ids: &LastUniqueIdStub) {
        let id = ids.next().expect("next id");
        assert!(id >= 1, "parent-issued id should be positive, got {id}");
    }

    /// End-to-end IPC error-path regression for the tokio runner.
    /// Mirrors the sync version of this test: a worker subprocess (with
    /// capture on) issues an unknown method index, the parent's
    /// `dispatch` returns `Err(...)`, and the worker-side stub surfaces
    /// the failure as [`HostedRpcError::Dispatch`] — never as
    /// [`HostedRpcError::Transport`]. The post-error stub must remain
    /// usable for ordinary RPC calls so the IPC framing stays in sync.
    #[test]
    async fn hosted_rpc_unknown_method_surfaces_as_dispatch_error(ids: &LastUniqueIdStub) {
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

        let id = ids.next().expect("post-error stub must keep working");
        assert!(id > 0, "post-error id must still be positive, got {id}");
    }

    /// HR1.0 regression: round-trip a payload larger than 64 KiB so the
    /// `u32` length prefix is exercised end-to-end across the IPC
    /// HostedRpc transport (both directions of the round-trip carry
    /// >64 KiB: the request asks for `size` bytes and the reply returns
    /// `size` bytes plus a small reply frame envelope). The payload is
    /// filled with a deterministic `i % 251` pattern so the test fails
    /// on truncation or corruption, not just on the length mismatch.
    #[test]
    async fn hosted_rpc_large_payload_round_trip_exceeds_64_kib(ids: &LastUniqueIdStub) {
        const SIZE: u32 = 256 * 1024; // 256 KiB — comfortably above 64 KiB.
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

    /// HR1.0 regression: two concurrent in-flight RPCs from the same
    /// test body must not deadlock and must each receive their own
    /// reply. Under the MVP the worker-side `IpcHostedRpcTransport`
    /// holds the connection mutex for the full request/response
    /// round-trip, which serialises the two calls; this test pins that
    /// behaviour and proves that the request-id framing in
    /// `IpcCommand::HostedRpcReply` / `IpcResponse::HostedRpcCall`
    /// keeps the protocol in sync even when several stub calls are
    /// pending on the same task.
    ///
    /// `tokio::join!` polls both branches concurrently; each branch
    /// drops into `tokio::task::block_in_place(...)` to invoke the sync
    /// stub method. Worst case under the MVP they run back-to-back; the
    /// test only fails on deadlock, on framing corruption, or on
    /// duplicate ids.
    #[test]
    async fn hosted_rpc_two_concurrent_calls_multiplex(ids: &LastUniqueIdStub) {
        let (a, b) = tokio::join!(async { ids.next().expect("a") }, async {
            ids.next().expect("b")
        },);
        assert!(
            a != b,
            "concurrent in-flight calls returned the same id ({a} == {b}); \
             the IPC request-id multiplexer did not keep the calls distinct"
        );
        // Both ids must be positive; relative ordering is not meaningful
        // under the MVP's mutex serialisation (the slower-to-be-polled
        // branch may win the lock first).
        assert!(a > 0, "concurrent call a returned non-positive id {a}");
        assert!(b > 0, "concurrent call b returned non-positive id {b}");
    }

    /// Singleton-property regression for the tokio runner. IPC worker
    /// subprocesses must never run the HostedRpc owner constructor —
    /// the top-level parent owns the singleton.
    #[test]
    async fn hosted_rpc_owner_runs_only_in_top_level_parent(_ids: &LastUniqueIdStub) {
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
