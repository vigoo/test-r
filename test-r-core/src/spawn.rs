use crate::internal::PanicCause;
use crate::panic_hook;
use futures::FutureExt;
use std::future::Future;
use tokio::task::JoinHandle;

/// Spawn a future on the tokio runtime with test context propagation.
/// If the spawned task panics and the test uses `DetachedPanicPolicy::FailTest` (default),
/// the panic will be reported as a test failure after the test body completes.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let test_id = panic_hook::current_test_id();
    let collector = test_id.and_then(panic_hook::get_detached_collector);

    tokio::spawn(async move {
        if let Some(id) = test_id {
            panic_hook::set_current_test_id(id);
        }
        let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;
        match result {
            Ok(value) => value,
            Err(panic_payload) => {
                let cause = panic_hook::take_current_panic_capture().unwrap_or_else(|| {
                    let message = panic_payload
                        .downcast_ref::<String>()
                        .cloned()
                        .or(panic_payload.downcast_ref::<&str>().map(|s| s.to_string()));
                    PanicCause {
                        message,
                        location: None,
                        backtrace: None,
                    }
                });

                if let Some(collector) = &collector {
                    match collector.lock() {
                        Ok(mut panics) => panics.push(cause),
                        Err(poisoned) => poisoned.into_inner().push(cause),
                    }
                }

                std::panic::resume_unwind(panic_payload);
            }
        }
    })
}
