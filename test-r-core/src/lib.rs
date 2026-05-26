pub mod args;
pub mod bench;
mod execution;
pub mod internal;
mod ipc;
mod output;
mod panic_hook;
pub mod spawn;
mod stats;
#[cfg(feature = "tokio")]
mod tokio;
pub mod worker;

#[allow(dead_code)]
mod sync;

#[cfg(not(feature = "tokio"))]
pub use sync::test_runner;

#[cfg(feature = "tokio")]
pub use tokio::test_runner;

pub use worker::worker_index;

/// Re-export of [`desert_rust`] so that proc-macros emitted by
/// `test-r-macro` (e.g. [`test_r::hosted_rpc`]) can refer to it via
/// `::test_r::core::desert_rust::...` without forcing downstream
/// users to add `desert_rust` to their own `Cargo.toml`.
///
/// This is an internal support re-export — user code should not depend
/// on it being available at this path. The `#[doc(hidden)]` attribute
/// keeps it out of the rendered docs and signals that the surface
/// belongs to the macro support layer, not the public test-r API.
#[doc(hidden)]
pub use desert_rust;

// =====================================================================
// Hosted descriptor codec / worker reconstructor helpers.
//
// These two functions are the single place the descriptor-based Hosted
// dep wiring is built. The `tokio` cargo feature on `test-r-core`
// selects which variant of each function compiles, so that:
//
// - Under the **tokio** runtime, Hosted deps always use the *async*
//   path (`AsyncHostedDep::descriptor` / `from_descriptor`). Thanks to the
//   blanket `impl<T: HostedDep> AsyncHostedDep for T`, every sync `HostedDep`
//   automatically reaches the async path with no user-visible cost (the
//   bridged `from_descriptor` just wraps the sync impl in
//   `std::future::ready(...)`).
// - Under the **sync** runtime, Hosted deps always use the *sync* path
//   (`HostedDep::descriptor` / `from_descriptor`). This intentionally
//   keeps sync builds free of any block-poll machinery: a Hosted dep
//   that only implements `AsyncHostedDep` simply fails to compile
//   here, rather than panicking at runtime.
//
// The macro now emits a single uniform call to these helpers regardless
// of the (now deprecated) `async_worker` attribute; see
// `test-r-macro/src/deps.rs`.
// =====================================================================

/// **Hidden macro-support helper.**
///
/// Build the parent-side codec for a Hosted dep. Under the `tokio`
/// runtime this dispatches through [`internal::AsyncHostedDep`]; under
/// the sync runtime it dispatches through [`internal::HostedDep`]. The
/// returned [`internal::CloneableCodec`] knows how to:
///
/// - downcast the owner `Arc<dyn Any + Send + Sync>` back to `T` and
///   produce the descriptor bytes (`to_wire`), and
/// - turn raw descriptor bytes into a boxed payload that the matching
///   reconstructor below will hand off to `from_descriptor`
///   (`from_wire_bytes`).
///
/// Not part of the public API; only the proc-macro emits calls to it.
#[doc(hidden)]
#[cfg(feature = "tokio")]
pub fn __test_r_make_hosted_codec<T>() -> internal::CloneableCodec
where
    T: internal::AsyncHostedDep,
{
    use std::sync::Arc;
    internal::CloneableCodec {
        to_wire: Arc::new(|any: Arc<dyn std::any::Any + Send + Sync>| {
            let value: Arc<T> = any
                .downcast::<T>()
                .expect("Hosted dependency type mismatch in descriptor()");
            <T as internal::AsyncHostedDep>::descriptor(&*value)
        }),
        from_wire_bytes: Arc::new(|bytes: &[u8]| {
            // The "wire payload" for a Hosted dep is the raw descriptor
            // bytes; the matching reconstructor will run from_descriptor
            // against them on the worker side.
            let boxed: Arc<dyn std::any::Any + Send + Sync> = Arc::new(bytes.to_vec());
            boxed
        }),
    }
}

/// **Hidden macro-support helper.** Sync-runtime variant of
/// [`__test_r_make_hosted_codec`]; see that doc-comment.
#[doc(hidden)]
#[cfg(not(feature = "tokio"))]
pub fn __test_r_make_hosted_codec<T>() -> internal::CloneableCodec
where
    T: internal::HostedDep,
{
    use std::sync::Arc;
    internal::CloneableCodec {
        to_wire: Arc::new(|any: Arc<dyn std::any::Any + Send + Sync>| {
            let value: Arc<T> = any
                .downcast::<T>()
                .expect("Hosted dependency type mismatch in descriptor()");
            <T as internal::HostedDep>::descriptor(&*value)
        }),
        from_wire_bytes: Arc::new(|bytes: &[u8]| {
            let boxed: Arc<dyn std::any::Any + Send + Sync> = Arc::new(bytes.to_vec());
            boxed
        }),
    }
}

/// **Hidden macro-support helper.**
///
/// Build the worker-side [`internal::WorkerReconstructor`] for a Hosted
/// dep. Under the `tokio` runtime this returns an
/// [`internal::WorkerReconstructor::Async`] closure that awaits
/// [`internal::AsyncHostedDep::from_descriptor`]; under the sync
/// runtime it returns an [`internal::WorkerReconstructor::Sync`]
/// closure that calls [`internal::HostedDep::from_descriptor`].
///
/// The descriptor `Vec<u8>` is produced by the matching
/// [`__test_r_make_hosted_codec`] above; the two helpers must stay in
/// lockstep.
///
/// Not part of the public API; only the proc-macro emits calls to it.
#[doc(hidden)]
#[cfg(feature = "tokio")]
pub fn __test_r_make_hosted_worker_reconstructor<T>() -> internal::WorkerReconstructor
where
    T: internal::AsyncHostedDep,
{
    use std::sync::Arc;
    internal::WorkerReconstructor::Async(Arc::new(
        |wire_payload: Arc<dyn std::any::Any + Send + Sync>, _deps| {
            Box::pin(async move {
                let bytes: Arc<Vec<u8>> = wire_payload
                    .downcast::<Vec<u8>>()
                    .expect("Hosted worker reconstructor expected Vec<u8> descriptor payload");
                let value: T = <T as internal::AsyncHostedDep>::from_descriptor(&bytes).await;
                let boxed: Arc<dyn std::any::Any + Send + Sync> = Arc::new(value);
                boxed
            })
        },
    ))
}

/// **Hidden macro-support helper.** Sync-runtime variant of
/// [`__test_r_make_hosted_worker_reconstructor`]; see that
/// doc-comment.
#[doc(hidden)]
#[cfg(not(feature = "tokio"))]
pub fn __test_r_make_hosted_worker_reconstructor<T>() -> internal::WorkerReconstructor
where
    T: internal::HostedDep,
{
    use std::sync::Arc;
    internal::WorkerReconstructor::Sync(Arc::new(
        |wire_payload: Arc<dyn std::any::Any + Send + Sync>, _deps| {
            let bytes: Arc<Vec<u8>> = wire_payload
                .downcast::<Vec<u8>>()
                .expect("Hosted worker reconstructor expected Vec<u8> descriptor payload");
            let value: T = <T as internal::HostedDep>::from_descriptor(&bytes);
            let boxed: Arc<dyn std::any::Any + Send + Sync> = Arc::new(value);
            boxed
        },
    ))
}

// =====================================================================
// `worker = both(T)` helpers.
//
// `#[test_dep(scope = Hosted, worker = both(Trait))]` is lowered by the
// macro into two `RegisteredDependency` entries (one Hosted, one
// HostedRpc) that share a single parent-side owner via
// [`internal::HostedBothShared`]. The three helpers below centralize
// the shared logic:
//
// - `__test_r_make_hosted_both_shared::<T>(owner)` builds the cell
//   used by the macro's weak cache. Cfg-selected on `tokio` so the descriptor
//   call uses `AsyncHostedDep::descriptor` under tokio and
//   `HostedDep::descriptor` under sync, mirroring the single-view helpers.
// - `__test_r_make_hosted_both_codec()` produces the Hosted-view
//   codec; both bytes (`to_wire`) and payload (`from_wire_bytes`)
//   shapes match the existing single-view Hosted codec so the
//   runtime worker side stays unchanged.
// - `__test_r_make_hosted_both_rpc_factory::<T>()` produces the
//   HostedRpc-view factory. It downcasts the shared cell to extract
//   the inner `Arc<HostedRpcOwnerCell>`, and reuses the same
//   `build_stub(channel)` path the legacy HostedRpc factory uses.
// =====================================================================

/// **Hidden macro-support helper.** Build the shared owner cell for
/// a `worker = both(T)` dep. Tokio variant: descriptor is computed
/// via [`internal::AsyncHostedDep::descriptor`] and the RPC cell is
/// constructed via [`internal::HostedRpcOwnerCell::from_async_owner`]
/// so both sync and async HostedRpc owners flow through the same
/// async dispatch path.
#[doc(hidden)]
#[cfg(feature = "tokio")]
pub fn __test_r_make_hosted_both_shared<T>(owner: T) -> internal::HostedBothShared
where
    T: internal::AsyncHostedDep + internal::AsyncHostedRpcDep,
{
    use std::sync::Arc;
    let descriptor_bytes = <T as internal::AsyncHostedDep>::descriptor(&owner);
    let rpc_cell = Arc::new(internal::HostedRpcOwnerCell::from_async_owner(owner));
    internal::HostedBothShared::new(descriptor_bytes, rpc_cell)
}

/// **Hidden macro-support helper.** Sync-runtime variant of
/// [`__test_r_make_hosted_both_shared`]; descriptor is computed via
/// [`internal::HostedDep::descriptor`].
#[doc(hidden)]
#[cfg(not(feature = "tokio"))]
pub fn __test_r_make_hosted_both_shared<T>(owner: T) -> internal::HostedBothShared
where
    T: internal::HostedDep + internal::HostedRpcDep,
{
    use std::sync::Arc;
    let descriptor_bytes = <T as internal::HostedDep>::descriptor(&owner);
    let rpc_cell = Arc::new(internal::HostedRpcOwnerCell::from_owner(owner));
    internal::HostedBothShared::new(descriptor_bytes, rpc_cell)
}

/// **Hidden macro-support helper.** Wrap a HostedRpc owner value into a
/// [`internal::HostedRpcOwnerCell`]. The tokio variant goes through
/// [`internal::HostedRpcOwnerCell::from_async_owner`] so async owners
/// are dispatched asynchronously; the sync variant uses the back-compat
/// sync constructor. Used by the `#[test_dep(scope = HostedRpc)]`
/// lowering so the choice of async vs sync cell happens in one place.
#[doc(hidden)]
#[cfg(feature = "tokio")]
pub fn __test_r_make_hosted_rpc_cell<T>(owner: T) -> internal::HostedRpcOwnerCell
where
    T: internal::AsyncHostedRpcDep,
{
    internal::HostedRpcOwnerCell::from_async_owner(owner)
}

/// **Hidden macro-support helper.** Sync-runtime variant of
/// [`__test_r_make_hosted_rpc_cell`].
#[doc(hidden)]
#[cfg(not(feature = "tokio"))]
pub fn __test_r_make_hosted_rpc_cell<T>(owner: T) -> internal::HostedRpcOwnerCell
where
    T: internal::HostedRpcDep,
{
    internal::HostedRpcOwnerCell::from_owner(owner)
}

/// **Hidden macro-support helper.** Hosted-view codec for the `both`
/// variant. Wire format is identical to the existing single-view
/// Hosted codec so the worker reconstructor (still
/// [`__test_r_make_hosted_worker_reconstructor`]) stays unchanged.
#[doc(hidden)]
pub fn __test_r_make_hosted_both_codec() -> internal::CloneableCodec {
    use std::any::Any;
    use std::sync::Arc;
    internal::CloneableCodec {
        to_wire: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            let shared: Arc<internal::HostedBothShared> = any
                .downcast::<internal::HostedBothShared>()
                .expect("HostedBothShared downcast failed in both-codec to_wire");
            shared.descriptor_bytes().to_vec()
        }),
        from_wire_bytes: Arc::new(|bytes: &[u8]| {
            // Same payload shape as the single-view Hosted codec: a
            // boxed `Vec<u8>` for the worker reconstructor to consume.
            let boxed: Arc<dyn Any + Send + Sync> = Arc::new(bytes.to_vec());
            boxed
        }),
    }
}

/// **Hidden macro-support helper.** HostedRpc-view factory for the
/// `both` variant. Pulls the inner `Arc<HostedRpcOwnerCell>` out of
/// the shared cell and reuses the user's
/// [`internal::HostedRpcDep::build_stub`] for the worker stub.
#[doc(hidden)]
#[cfg(feature = "tokio")]
pub fn __test_r_make_hosted_both_rpc_factory<T, Stub>() -> internal::RpcFactory
where
    T: internal::AsyncHostedRpcDep<Stub = Stub>,
    Stub: Send + Sync + 'static,
{
    use std::any::Any;
    use std::sync::Arc;
    internal::RpcFactory {
        owner_into_cell: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            let shared: Arc<internal::HostedBothShared> = any
                .downcast::<internal::HostedBothShared>()
                .expect("HostedBothShared downcast failed in both-rpc-factory owner_into_cell");
            shared.rpc_cell()
        }),
        build_stub: Arc::new(|channel: internal::HostedRpcChannel| {
            let stub: Stub = <T as internal::AsyncHostedRpcDep>::build_stub(channel);
            let boxed: Arc<dyn Any + Send + Sync> = Arc::new(stub);
            boxed
        }),
    }
}

/// **Hidden macro-support helper.** Sync-runtime variant of
/// [`__test_r_make_hosted_both_rpc_factory`]. The `build_stub` is sourced
/// from [`internal::HostedRpcDep::build_stub`] because the sync runtime
/// cannot drive `AsyncHostedRpcDep` owners.
#[doc(hidden)]
#[cfg(not(feature = "tokio"))]
pub fn __test_r_make_hosted_both_rpc_factory<T, Stub>() -> internal::RpcFactory
where
    T: internal::HostedRpcDep<Stub = Stub>,
    Stub: Send + Sync + 'static,
{
    use std::any::Any;
    use std::sync::Arc;
    internal::RpcFactory {
        owner_into_cell: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            let shared: Arc<internal::HostedBothShared> = any
                .downcast::<internal::HostedBothShared>()
                .expect("HostedBothShared downcast failed in both-rpc-factory owner_into_cell");
            shared.rpc_cell()
        }),
        build_stub: Arc::new(|channel: internal::HostedRpcChannel| {
            let stub: Stub = <T as internal::HostedRpcDep>::build_stub(channel);
            let boxed: Arc<dyn Any + Send + Sync> = Arc::new(stub);
            boxed
        }),
    }
}

/// **Hidden macro-support helper.** Build a `RpcFactory` for the
/// stand-alone `scope = HostedRpc` lowering (no `both(T)` companion).
/// Tokio variant goes through [`internal::AsyncHostedRpcDep::build_stub`]
/// so async owners flow through one entry point.
#[doc(hidden)]
#[cfg(feature = "tokio")]
pub fn __test_r_make_hosted_rpc_factory<T, Stub>() -> internal::RpcFactory
where
    T: internal::AsyncHostedRpcDep<Stub = Stub>,
    Stub: Send + Sync + 'static,
{
    use std::any::Any;
    use std::sync::Arc;
    internal::RpcFactory {
        owner_into_cell: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            any.downcast::<internal::HostedRpcOwnerCell>()
                .expect("HostedRpc owner downcast to HostedRpcOwnerCell failed")
        }),
        build_stub: Arc::new(|channel: internal::HostedRpcChannel| {
            let stub: Stub = <T as internal::AsyncHostedRpcDep>::build_stub(channel);
            let boxed: Arc<dyn Any + Send + Sync> = Arc::new(stub);
            boxed
        }),
    }
}

/// **Hidden macro-support helper.** Sync-runtime variant of
/// [`__test_r_make_hosted_rpc_factory`].
#[doc(hidden)]
#[cfg(not(feature = "tokio"))]
pub fn __test_r_make_hosted_rpc_factory<T, Stub>() -> internal::RpcFactory
where
    T: internal::HostedRpcDep<Stub = Stub>,
    Stub: Send + Sync + 'static,
{
    use std::any::Any;
    use std::sync::Arc;
    internal::RpcFactory {
        owner_into_cell: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            any.downcast::<internal::HostedRpcOwnerCell>()
                .expect("HostedRpc owner downcast to HostedRpcOwnerCell failed")
        }),
        build_stub: Arc::new(|channel: internal::HostedRpcChannel| {
            let stub: Stub = <T as internal::HostedRpcDep>::build_stub(channel);
            let boxed: Arc<dyn Any + Send + Sync> = Arc::new(stub);
            boxed
        }),
    }
}

#[cfg(test)]
mod hosted_helper_tests {
    //! Exercise the feature-gated
    //! [`__test_r_make_hosted_codec`] /
    //! [`__test_r_make_hosted_worker_reconstructor`] helpers end to
    //! end against a tiny `HostedDep` fixture.
    //!
    //! Both helper variants must reject `WorkerReconstructor::Sync`
    //! vs `Async` choice at the cargo-feature level, so we keep this
    //! test cfg-aware: it asserts the matching variant under each
    //! feature.
    use super::*;
    use std::any::Any;
    use std::sync::Arc;

    /// Minimal sync `HostedDep` fixture. Under the `tokio` feature
    /// the blanket bridge also makes it `AsyncHostedDep`, so the same fixture
    /// is usable against both helper variants.
    #[derive(Debug, PartialEq, Eq)]
    struct Fixture {
        bytes: Vec<u8>,
    }

    impl internal::HostedDep for Fixture {
        fn descriptor(&self) -> Vec<u8> {
            self.bytes.clone()
        }
        fn from_descriptor(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
            }
        }
    }

    #[test]
    fn make_hosted_codec_round_trips_descriptor_bytes() {
        let codec = __test_r_make_hosted_codec::<Fixture>();
        let owner: Arc<dyn Any + Send + Sync> = Arc::new(Fixture {
            bytes: vec![1, 2, 3, 4],
        });

        let wire_bytes = (codec.to_wire)(owner);
        assert_eq!(wire_bytes, vec![1, 2, 3, 4]);

        let wire_payload = (codec.from_wire_bytes)(&wire_bytes);
        let recovered_bytes: Arc<Vec<u8>> = wire_payload
            .downcast::<Vec<u8>>()
            .expect("from_wire_bytes must produce Arc<Vec<u8>>");
        assert_eq!(*recovered_bytes, vec![1, 2, 3, 4]);
    }

    /// Under the tokio runtime, the worker reconstructor helper must
    /// return [`internal::WorkerReconstructor::Async`].
    #[cfg(feature = "tokio")]
    #[test]
    fn make_hosted_worker_reconstructor_is_async_under_tokio() {
        let recon = __test_r_make_hosted_worker_reconstructor::<Fixture>();
        match recon {
            internal::WorkerReconstructor::Async(_) => {}
            internal::WorkerReconstructor::Sync(_) => panic!(
                "tokio build must produce a WorkerReconstructor::Async for Hosted deps; got Sync"
            ),
        }
    }

    /// Under the sync runtime, the worker reconstructor helper must
    /// return [`internal::WorkerReconstructor::Sync`] so the sync
    /// runner can drive it without any block-poll machinery.
    #[cfg(not(feature = "tokio"))]
    #[test]
    fn make_hosted_worker_reconstructor_is_sync_under_sync_runtime() {
        let recon = __test_r_make_hosted_worker_reconstructor::<Fixture>();
        match recon {
            internal::WorkerReconstructor::Sync(_) => {}
            internal::WorkerReconstructor::Async(_) => panic!(
                "sync build must produce a WorkerReconstructor::Sync for Hosted deps; got Async"
            ),
        }
    }

    /// Drive the sync-runtime reconstructor closure end to end on
    /// the matching descriptor bytes the codec produces. Pinned only
    /// for the sync build because the tokio build returns an Async
    /// closure that needs a runtime to await.
    #[cfg(not(feature = "tokio"))]
    #[test]
    fn sync_worker_reconstructor_rebuilds_fixture_from_descriptor() {
        // Re-use a small `DependencyView` impl: the helper's worker
        // closure ignores the view, so we pass an empty stub.
        #[derive(Debug)]
        struct EmptyView;
        impl internal::DependencyView for EmptyView {
            fn get(&self, _name: &str) -> Option<Arc<dyn Any + Send + Sync>> {
                None
            }
        }

        let codec = __test_r_make_hosted_codec::<Fixture>();
        let recon = __test_r_make_hosted_worker_reconstructor::<Fixture>();

        let owner: Arc<dyn Any + Send + Sync> = Arc::new(Fixture {
            bytes: vec![5, 6, 7],
        });
        let wire_bytes = (codec.to_wire)(owner);
        let payload = (codec.from_wire_bytes)(&wire_bytes);

        let deps: Arc<dyn internal::DependencyView + Send + Sync> = Arc::new(EmptyView);
        let rebuilt = match recon {
            internal::WorkerReconstructor::Sync(f) => f(payload, deps),
            internal::WorkerReconstructor::Async(_) => unreachable!(
                "sync build cannot return Async; pinned by \
                 make_hosted_worker_reconstructor_is_sync_under_sync_runtime",
            ),
        };
        let rebuilt: Arc<Fixture> = rebuilt
            .downcast::<Fixture>()
            .expect("worker reconstructor must produce the original Hosted dep type");
        assert_eq!(
            *rebuilt,
            Fixture {
                bytes: vec![5, 6, 7]
            }
        );
    }

    // -----------------------------------------------------------------
    // `worker = both(T)` helper tests.
    //
    // The macro lowering for `#[test_dep(scope = Hosted, worker =
    // both(Trait))]` is exercised end-to-end by the
    // `sharing::hosted_both_basic` example fixtures. These unit tests
    // pin the three pieces of cargo-feature-aware glue in this file:
    //
    // - `__test_r_make_hosted_both_shared::<T>(owner)` builds the
    //   shared cell that the macro's weak cache hands back to both
    //   the Hosted and HostedRpc registrations.
    // - `__test_r_make_hosted_both_codec()` serializes the cached
    //   descriptor bytes for the Hosted view.
    // - `__test_r_make_hosted_both_rpc_factory::<T>()` extracts the
    //   inner `Arc<HostedRpcOwnerCell>` for the HostedRpc view and
    //   builds the worker-side stub via `HostedRpcDep::build_stub`.
    // -----------------------------------------------------------------

    /// Minimal `HostedDep + HostedRpcDep` fixture for helper tests. The id
    /// allocator stands in for any tiny control surface; the bytes field doubles
    /// as the descriptor.
    #[derive(Debug)]
    struct BothFixture {
        bytes: Vec<u8>,
        counter: std::sync::Mutex<u64>,
    }

    impl BothFixture {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                counter: std::sync::Mutex::new(0),
            }
        }
    }

    impl internal::HostedDep for BothFixture {
        fn descriptor(&self) -> Vec<u8> {
            self.bytes.clone()
        }
        fn from_descriptor(bytes: &[u8]) -> Self {
            Self::new(bytes.to_vec())
        }
    }

    /// Stub view for the BothFixture. Holds a HostedRpcChannel so a
    /// realistic build_stub round-trip is exercised; the tests below
    /// just verify the factory hands back a usable `Arc<BothStub>`,
    /// not a full IPC round-trip (covered by the example fixtures).
    pub struct BothStub {
        _channel: internal::HostedRpcChannel,
    }

    impl internal::HostedRpcDep for BothFixture {
        type Stub = BothStub;
        fn dispatch(&mut self, method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
            // Single method: bump the counter and return its value.
            if method_idx == 1 {
                let mut g = self.counter.lock().map_err(|e| e.to_string())?;
                *g += 1;
                Ok(g.to_be_bytes().to_vec())
            } else {
                Err(format!("BothFixture: unknown method_idx {method_idx}"))
            }
        }
        fn build_stub(channel: internal::HostedRpcChannel) -> Self::Stub {
            BothStub { _channel: channel }
        }
    }

    /// `__test_r_make_hosted_both_shared(owner)` captures the
    /// owner's descriptor bytes once and wraps the owner in a
    /// `HostedRpcOwnerCell`. The descriptor must match the owner's
    /// `HostedDep::descriptor()` output exactly.
    #[test]
    fn make_hosted_both_shared_captures_descriptor_bytes() {
        let owner = BothFixture::new(vec![10, 20, 30]);
        let shared = __test_r_make_hosted_both_shared::<BothFixture>(owner);
        assert_eq!(shared.descriptor_bytes(), &[10, 20, 30]);
        // The owner cell must be live: a dispatch call must succeed
        // (the closure runs the method without panicking and returns
        // a non-empty reply). Under the tokio feature the cell is the
        // async variant — drive it through `dispatch_async` on a
        // tokio runtime; under the sync feature the cell is sync.
        let reply =
            dispatch_cell_for_test(&shared.rpc_cell(), 1, &[]).expect("dispatch must succeed");
        assert_eq!(reply, 1u64.to_be_bytes().to_vec());
    }

    /// Helper: dispatch on a `HostedRpcOwnerCell` whose async/sync
    /// variant depends on the active cargo feature. Used by the
    /// `worker = both(T)` helper tests so the test bodies don't need
    /// to know whether the cell was built via `from_owner` or
    /// `from_async_owner` — both surface the same per-call result.
    fn dispatch_cell_for_test(
        cell: &internal::HostedRpcOwnerCell,
        method_idx: u32,
        args: &[u8],
    ) -> Result<Vec<u8>, String> {
        #[cfg(feature = "tokio")]
        {
            // `tokio` resolves to the local `mod tokio` inside this
            // crate; reach the external crate with `::tokio`.
            let rt = ::tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime");
            rt.block_on(cell.dispatch_async(method_idx, args))
        }
        #[cfg(not(feature = "tokio"))]
        {
            cell.dispatch(method_idx, args)
        }
    }

    /// `__test_r_make_hosted_both_codec().to_wire` downcasts the
    /// shared cell and returns its captured descriptor bytes — the
    /// same shape the worker-side reconstructor expects.
    #[test]
    fn make_hosted_both_codec_serializes_descriptor_bytes() {
        let codec = __test_r_make_hosted_both_codec();
        let shared: Arc<internal::HostedBothShared> =
            Arc::new(__test_r_make_hosted_both_shared::<BothFixture>(
                BothFixture::new(vec![1, 2, 3, 4]),
            ));
        let arc_any: Arc<dyn Any + Send + Sync> = shared;

        let wire_bytes = (codec.to_wire)(arc_any);
        assert_eq!(wire_bytes, vec![1, 2, 3, 4]);

        // `from_wire_bytes` must produce the same `Arc<Vec<u8>>`
        // payload shape the existing Hosted reconstructor consumes —
        // otherwise the worker side would not be able to reuse the
        // standard `__test_r_make_hosted_worker_reconstructor`.
        let wire_payload = (codec.from_wire_bytes)(&wire_bytes);
        let recovered_bytes: Arc<Vec<u8>> = wire_payload
            .downcast::<Vec<u8>>()
            .expect("from_wire_bytes must produce Arc<Vec<u8>>");
        assert_eq!(*recovered_bytes, vec![1, 2, 3, 4]);
    }

    /// `__test_r_make_hosted_both_rpc_factory::<T, Stub>().owner_into_cell`
    /// reaches into the shared cell, hands back the inner
    /// `Arc<HostedRpcOwnerCell>`, and a dispatched call hits the
    /// real owner (proven by the counter incrementing).
    #[test]
    fn make_hosted_both_rpc_factory_extracts_owner_cell() {
        let factory = __test_r_make_hosted_both_rpc_factory::<BothFixture, BothStub>();
        let shared: Arc<internal::HostedBothShared> = Arc::new(__test_r_make_hosted_both_shared::<
            BothFixture,
        >(BothFixture::new(vec![])));
        let arc_any: Arc<dyn Any + Send + Sync> = shared.clone();

        let cell = (factory.owner_into_cell)(arc_any);
        assert!(
            Arc::ptr_eq(&cell, &shared.rpc_cell()),
            "factory must return the exact same inner HostedRpcOwnerCell Arc the \
             shared cell holds; otherwise the RPC view would dispatch against a \
             different owner than the descriptor view captured"
        );

        // The cell is functional: dispatch routes to the real owner
        // method (counter starts at 0; first call must yield 1). Same
        // cargo-feature-aware dispatch as
        // `make_hosted_both_shared_captures_descriptor_bytes` since the
        // underlying cell shares the same async/sync split.
        let reply = dispatch_cell_for_test(&cell, 1, &[]).expect("dispatch must succeed");
        assert_eq!(reply, 1u64.to_be_bytes().to_vec());
    }

    /// `__test_r_make_hosted_both_rpc_factory::<T, Stub>().build_stub`
    /// constructs a worker-side `Stub` via the user's
    /// `HostedRpcDep::build_stub` and boxes it as `Arc<dyn Any>` so
    /// the runtime can route it through the standard dep view.
    #[test]
    fn make_hosted_both_rpc_factory_builds_stub() {
        use internal::{HostedRpcChannel, HostedRpcError, HostedRpcTransport};

        // Minimal in-process transport stand-in: every call returns
        // the unit reply. We don't actually call into it; the test
        // just exercises that build_stub produces a downcastable
        // `Arc<dyn Any>` carrying the channel.
        struct DummyTransport;
        impl HostedRpcTransport for DummyTransport {
            fn call(
                &self,
                _dep_id: &str,
                _method_idx: u32,
                _args: Vec<u8>,
            ) -> Result<Vec<u8>, HostedRpcError> {
                Ok(Vec::new())
            }
        }

        let factory = __test_r_make_hosted_both_rpc_factory::<BothFixture, BothStub>();
        let transport: Arc<dyn HostedRpcTransport> = Arc::new(DummyTransport);
        let channel = HostedRpcChannel::new("test::both_fixture".to_string(), transport);

        let stub_any: Arc<dyn Any + Send + Sync> = (factory.build_stub)(channel);
        let _stub: Arc<BothStub> = stub_any
            .downcast::<BothStub>()
            .expect("build_stub must produce the BothFixture::Stub type");
    }
}
