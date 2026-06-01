//! Example: a `HostedRpc` `#[test_dep]` whose owner emits host-side
//! output (both from a background thread spawned in the constructor
//! and from inside `dispatch`). Used by
//! [`tests::host_capture`](../../../tests/host_capture.rs) to assert
//! the parent-side host-capture path attributes the produced lines to
//! the test(s) whose window contains them.
//!
//! Lives in a sibling module of the other HostedRpc examples so the
//! integration test can target only this binary's tests via a name
//! filter without dragging the rest of the suite along.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
    use test_r::core::{HostedRpcChannel, HostedRpcDep, HostedRpcError};
    use test_r::{test, test_dep};

    /// Marker the constructor stamps onto stdout from a spawned
    /// background thread. The integration test greps for this in the
    /// `[host]` lines surfaced under one of the tests in this module.
    pub const BG_PRINT_MARKER: &str = "HOST_BG_THREAD_TICK";

    /// Marker the dispatcher prints on every method call. The
    /// integration test asserts at least one of these lines lands
    /// under the test that triggered the call.
    pub const DISPATCH_PRINT_MARKER: &str = "HOST_DISPATCH_HIT";

    pub struct HostNoisyOwner {
        _bg_stop: Arc<AtomicBool>,
        _bg_handle: Option<thread::JoinHandle<()>>,
    }

    impl HostNoisyOwner {
        fn new() -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let stop_clone = stop.clone();
            let handle = thread::Builder::new()
                .name("host-capture-demo-bg".to_string())
                .spawn(move || {
                    // Emit one marker line every ~20ms so several
                    // land inside any reasonable per-test window even
                    // for very short tests.
                    while !stop_clone.load(Ordering::SeqCst) {
                        println!("{BG_PRINT_MARKER}");
                        thread::sleep(Duration::from_millis(20));
                    }
                })
                .expect("spawn host-capture demo bg thread");
            Self {
                _bg_stop: stop,
                _bg_handle: Some(handle),
            }
        }
    }

    impl Drop for HostNoisyOwner {
        fn drop(&mut self) {
            self._bg_stop.store(true, Ordering::SeqCst);
            if let Some(h) = self._bg_handle.take() {
                let _ = h.join();
            }
        }
    }

    const METHOD_TOUCH: u32 = 1;

    impl HostedRpcDep for HostNoisyOwner {
        type Stub = HostNoisyStub;

        fn dispatch(&mut self, method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
            match method_idx {
                METHOD_TOUCH => {
                    // Synchronous print from inside the parent-side
                    // dispatcher. Because dispatch runs in the parent
                    // process (the whole point of HostedRpc), this
                    // goes through the host-capture pipe rather than
                    // any worker stdout/stderr pipe.
                    println!("{DISPATCH_PRINT_MARKER}");
                    Ok(Vec::new())
                }
                other => Err(format!("HostNoisyOwner: unknown method_idx {other}")),
            }
        }

        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            HostNoisyStub { channel }
        }
    }

    pub struct HostNoisyStub {
        channel: HostedRpcChannel,
    }

    impl HostNoisyStub {
        pub fn touch(&self) -> Result<(), HostedRpcError> {
            self.channel.call(METHOD_TOUCH, Vec::new())?;
            Ok(())
        }
    }

    #[test_dep(scope = HostedRpc, stub = HostNoisyStub)]
    fn host_noisy_owner() -> HostNoisyOwner {
        HostNoisyOwner::new()
    }

    /// Triggers at least one dispatch print and gives the bg thread
    /// time to tick a couple of times so the integration test sees
    /// both `HOST_BG_THREAD_TICK` and `HOST_DISPATCH_HIT` lines
    /// attributed to this test's window.
    #[test]
    fn host_capture_demo_emits_both_markers(noisy: &HostNoisyStub) {
        for _ in 0..3 {
            noisy.touch().expect("touch must succeed");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
