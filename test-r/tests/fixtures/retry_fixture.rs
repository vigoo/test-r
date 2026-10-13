use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use test_r::core::{DynamicTestRegistration, FlakinessControl, TestProperties};
use test_r::{flaky, non_flaky, test, test_gen};

test_r::enable!();

// Ignored by ordinary cargo test runs: the regression test explicitly enables
// these fixtures, including the intentional persistent failures.
#[test]
#[ignore]
#[flaky(3)]
fn sync_recovers() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    assert!(CALLS.fetch_add(1, Ordering::SeqCst) >= 1, "first attempt");
}

#[test]
#[ignore]
#[flaky(3)]
fn sync_immediate() {}

#[test]
#[ignore]
#[flaky(3)]
fn sync_exhausts() {
    panic!("persistent failure");
}

#[test]
#[ignore]
#[non_flaky(3)]
fn sync_proves() {}

#[test_gen]
fn dynamic_sync(r: &mut DynamicTestRegistration) {
    for (name, failures) in [("recovers", 1), ("immediate", 0), ("exhausts", 3)] {
        let calls = Arc::new(AtomicUsize::new(0));
        r.add_sync_test(
            name,
            TestProperties {
                is_ignored: true,
                flakiness_control: FlakinessControl::RetryKnownFlaky(3),
                ..TestProperties::default()
            },
            None,
            move |_| {
                assert!(
                    calls.fetch_add(1, Ordering::SeqCst) >= failures,
                    "attempt failed"
                );
            },
        );
    }
}

#[cfg(feature = "tokio")]
mod asynchronous {
    use super::*;
    use test_r::test;

    #[test]
    #[ignore]
    #[flaky(3)]
    async fn async_recovers() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        assert!(CALLS.fetch_add(1, Ordering::SeqCst) >= 1, "first attempt");
    }

    #[test]
    #[ignore]
    #[flaky(3)]
    async fn async_immediate() {}

    #[test]
    #[ignore]
    #[flaky(3)]
    async fn async_exhausts() {
        panic!("persistent failure");
    }

    #[test]
    #[ignore]
    #[non_flaky(3)]
    async fn async_proves() {}

    #[test_gen]
    async fn dynamic_async(r: &mut DynamicTestRegistration) {
        for (name, failures) in [("recovers", 1), ("immediate", 0), ("exhausts", 3)] {
            let calls = Arc::new(AtomicUsize::new(0));
            r.add_async_test(
                name,
                TestProperties {
                    is_ignored: true,
                    flakiness_control: FlakinessControl::RetryKnownFlaky(3),
                    ..TestProperties::default()
                },
                None,
                move |_| {
                    let calls = calls.clone();
                    Box::pin(async move {
                        assert!(
                            calls.fetch_add(1, Ordering::SeqCst) >= failures,
                            "attempt failed"
                        );
                    })
                },
            );
        }
    }
}
