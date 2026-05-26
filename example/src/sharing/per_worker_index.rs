//! Example: `PerWorker` dep that seeds itself from `test_r::worker_index()`.
//!
//! Phase 3.3: PerWorker constructors can observe the zero-based worker index
//! the parent assigned to them via [`test_r::worker_index`]. This lets each
//! worker partition a global identifier space without coordination, which is
//! exactly what golem's `LastUniqueId` needs once it stops being a `Shared`
//! dep.
//!
//! The non-spawn-workers paths (`--nocapture`, no captured-output
//! requirement, or `Shared`-forced single-threaded fallback) all observe
//! worker index `0`.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU16, Ordering};
    use test_r::{test, test_dep};

    /// Mirrors the shape of `golem-worker-executor-test-utils::LastUniqueId`.
    pub struct LastUniqueId {
        pub id: AtomicU16,
    }

    /// PerWorker dep that reserves the high 8 bits of the id space for the
    /// worker index. With up to 256 workers and up to 256 ids per worker,
    /// the namespace stays collision-free as long as no single worker
    /// allocates more than 256 ids. We fail fast rather than silently mask
    /// if the worker index would not fit, because silent masking would
    /// re-introduce cross-worker id collisions.
    #[test_dep(scope = PerWorker)]
    fn last_unique_id() -> LastUniqueId {
        let worker_idx = test_r::worker_index();
        let worker = u8::try_from(worker_idx).expect(
            "LastUniqueId reserves 8 bits for the worker index; \
             run with --test-threads <= 256 or widen the id type",
        );
        let seed = u16::from(worker) << 8;
        println!(
            "PER_WORKER_INDEX: materializing LastUniqueId(seed={seed:#06x}, worker_idx={worker_idx})"
        );
        LastUniqueId {
            id: AtomicU16::new(seed),
        }
    }

    /// Pure check that the parent-side observation (no spawn-workers in
    /// this scope) is 0.
    fn assert_low_byte_is_worker_local(observed: u16, worker_idx: usize) {
        let expected_high = (worker_idx as u16 & 0xFF) << 8;
        // Strip the per-worker sequence bits; what remains must match the
        // worker's reserved namespace.
        assert_eq!(
            observed & 0xFF00,
            expected_high,
            "id {observed:#06x} escaped its per-worker namespace \
             (expected high byte {expected_high:#06x}, worker_idx={worker_idx})"
        );
    }

    #[test]
    fn worker_index_seeds_namespace_a(id: &LastUniqueId) {
        let worker_idx = test_r::worker_index();
        let value = id.id.fetch_add(1, Ordering::Relaxed);
        assert_low_byte_is_worker_local(value, worker_idx);
        println!("worker_index_seeds_namespace_a: id={value:#06x}, worker_idx={worker_idx}");
    }

    #[test]
    fn worker_index_seeds_namespace_b(id: &LastUniqueId) {
        let worker_idx = test_r::worker_index();
        let value = id.id.fetch_add(1, Ordering::Relaxed);
        assert_low_byte_is_worker_local(value, worker_idx);
        println!("worker_index_seeds_namespace_b: id={value:#06x}, worker_idx={worker_idx}");
    }

    #[test]
    fn worker_index_seeds_namespace_c(id: &LastUniqueId) {
        let worker_idx = test_r::worker_index();
        let value = id.id.fetch_add(1, Ordering::Relaxed);
        assert_low_byte_is_worker_local(value, worker_idx);
        println!("worker_index_seeds_namespace_c: id={value:#06x}, worker_idx={worker_idx}");
    }

    #[test]
    fn worker_index_seeds_namespace_d(id: &LastUniqueId) {
        let worker_idx = test_r::worker_index();
        let value = id.id.fetch_add(1, Ordering::Relaxed);
        assert_low_byte_is_worker_local(value, worker_idx);
        println!("worker_index_seeds_namespace_d: id={value:#06x}, worker_idx={worker_idx}");
    }
}
