//! Regression test: a downstream `Hosted` dep that takes `&Owner` for
//! a `#[test_dep(scope = Hosted, worker = both(Trait))]` owner must
//! resolve cleanly on the parent side.
//!
//! Before the fix the macro stored an `Arc<HostedBothShared>` under the
//! owner dep key, while the generated owner getter unconditionally
//! downcasted to `Arc<OwnerType>`. Any parent-side consumer that read
//! `&Owner` back from the dependency map (for example to compute a
//! second Hosted dep's descriptor) panicked with
//! `Dependency type mismatch: Any { .. }` at
//! `collect_parent_shared_dependencies_async`.
//!
//! After the fix the owner getter unwraps the `HostedBothShared` cell
//! first and pulls the shared `Arc<OwnerType>` out, so downstream
//! Hosted deps that take `&Owner` work the same as for plain
//! `scope = Hosted` registrations.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use test_r::core::{HostedDep, HostedRpcChannel, HostedRpcDep};
    use test_r::{hosted_rpc, test, test_dep};

    static OWNER_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);
    static DERIVED_CTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// Minimal control trait — the `worker = both(...)` lowering needs
    /// a `#[hosted_rpc]` trait to derive the stub view; the trait's
    /// methods themselves do not need to do anything interesting for
    /// the regression scenario.
    #[hosted_rpc]
    pub trait BothOwnerControl {
        fn next_id(&self) -> u64;
    }

    pub struct BothOwner {
        seed: u32,
        counter: Mutex<u64>,
    }

    impl BothOwner {
        fn new() -> Self {
            Self {
                seed: 7,
                counter: Mutex::new(0),
            }
        }
    }

    impl HostedDep for BothOwner {
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

    impl BothOwnerControl for BothOwner {
        fn next_id(&self) -> u64 {
            let mut g = self.counter.lock().unwrap();
            *g += 1;
            *g
        }
    }

    impl HostedRpcDep for BothOwner {
        type Stub = BothOwnerControlStub;

        fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
            BothOwnerControlDispatch::dispatch_both_owner_control(self, method_idx, args)
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            BothOwnerControlStub::new(channel)
        }
    }

    #[test_dep(scope = Hosted, worker = both(BothOwnerControl))]
    fn both_owner() -> BothOwner {
        OWNER_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        BothOwner::new()
    }

    /// Downstream `Hosted` dep that depends on the `both`-registered
    /// owner. Resolving this on the parent side is exactly what
    /// triggered the original `Dependency type mismatch` panic
    /// because the parent-side dep map stored `HostedBothShared` for
    /// the owner while the generated owner getter expected
    /// `Arc<BothOwner>` directly.
    pub struct Derived {
        derived_seed: u32,
    }

    impl HostedDep for Derived {
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
    fn derived(o: &BothOwner) -> Derived {
        DERIVED_CTOR_RUNS.fetch_add(1, Ordering::SeqCst);
        Derived {
            derived_seed: o.seed.wrapping_mul(2) + 1,
        }
    }

    /// End-to-end regression: the very fact that this test compiles
    /// and runs proves the owner getter resolves through
    /// `HostedBothShared` correctly. The assertion checks the seed
    /// propagated through the parent-side `&BothOwner` resolution
    /// into `derived(...)`'s descriptor.
    #[test]
    async fn downstream_hosted_dep_reads_both_owner(o: &BothOwner, d: &Derived) {
        assert_eq!(o.seed, 7, "owner seed must round-trip from descriptor");
        assert_eq!(
            d.derived_seed,
            7u32.wrapping_mul(2) + 1,
            "derived seed must equal `owner.seed * 2 + 1`, proving the parent-side \
             owner getter handed back the correct `&BothOwner` for `derived(...)`",
        );
    }

    /// Smoke-checks the RPC view still works end-to-end alongside the
    /// downstream descriptor consumer — both halves of the
    /// `worker = both(...)` lowering remain wired up correctly after
    /// the parent-side getter fix.
    #[test]
    async fn rpc_view_still_works_alongside_downstream_hosted_consumer(
        ctrl: &BothOwnerControlStub,
    ) {
        let id = ctrl.next_id();
        assert!(
            id >= 1,
            "next_id must allocate monotonically from 1, got {id}"
        );
    }
}
