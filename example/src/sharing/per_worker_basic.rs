//! Example: a `PerWorker` `#[test_dep]`.
//!
//! With `scope = PerWorker` each worker child process materializes its own
//! instance of the dependency, so the suite stays parallel under
//! `--test-threads N` even when output capturing is on.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use test_r::{test, test_dep};

    // Phase 1A: `scope = "PerWorker"` selects per-worker materialisation.

    /// A unique identifier for the worker process that materialized this dep.
    /// Tests use it to confirm that distinct workers receive distinct
    /// instances.
    pub struct WorkerId(pub usize);

    static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

    #[test_dep(scope = PerWorker)]
    fn create_worker_id() -> WorkerId {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        println!("PER_WORKER: materializing WorkerId({id})");
        WorkerId(id)
    }

    #[test]
    fn per_worker_test_a(id: &WorkerId) {
        assert!(id.0 >= 1, "WorkerId must be initialised");
        println!("per_worker_test_a saw WorkerId({})", id.0);
    }

    #[test]
    fn per_worker_test_b(id: &WorkerId) {
        assert!(id.0 >= 1, "WorkerId must be initialised");
        println!("per_worker_test_b saw WorkerId({})", id.0);
    }

    #[test]
    fn per_worker_test_c(id: &WorkerId) {
        assert!(id.0 >= 1, "WorkerId must be initialised");
        println!("per_worker_test_c saw WorkerId({})", id.0);
    }

    #[test]
    fn per_worker_test_d(id: &WorkerId) {
        assert!(id.0 >= 1, "WorkerId must be initialised");
        println!("per_worker_test_d saw WorkerId({})", id.0);
    }
}
