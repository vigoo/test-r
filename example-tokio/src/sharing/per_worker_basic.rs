//! Example: a `PerWorker` `#[test_dep]` exercised by async tests.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use test_r::{test, test_dep};

    pub struct WorkerId(pub usize);

    static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

    #[test_dep(scope = PerWorker)]
    fn create_worker_id() -> WorkerId {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        println!("PER_WORKER: materializing WorkerId({id})");
        WorkerId(id)
    }

    #[test]
    async fn per_worker_test_a(id: &WorkerId) {
        assert!(id.0 >= 1);
        tokio::task::yield_now().await;
    }

    #[test]
    async fn per_worker_test_b(id: &WorkerId) {
        assert!(id.0 >= 1);
        tokio::task::yield_now().await;
    }
}
