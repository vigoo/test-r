//! Example: a `Cloneable` `#[test_dep]` exercised by async tests.

#[cfg(test)]
mod tests {
    use test_r::core::CloneableDep;
    use test_r::{test, test_dep};

    #[derive(Debug, PartialEq, Eq)]
    pub struct PrecomputedPayload {
        pub bytes: Vec<u8>,
    }

    impl CloneableDep for PrecomputedPayload {
        fn to_wire(&self) -> Vec<u8> {
            self.bytes.clone()
        }

        fn from_wire(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
            }
        }
    }

    // Cloneable owner constructors run once on the parent. In the tokio
    // runner the parent awaits async owner constructors via
    // `TestSuiteExecution::collect_cloneable_wire_bytes_async`, so the
    // constructor may be `async fn`.
    #[test_dep(scope = Cloneable)]
    async fn create_payload() -> PrecomputedPayload {
        println!("CLONEABLE: building payload in parent (async)");
        // Show that we really do await an async future on the parent side.
        tokio::task::yield_now().await;
        PrecomputedPayload {
            bytes: (0..=255u8).collect(),
        }
    }

    #[test]
    async fn cloneable_test_a(payload: &PrecomputedPayload) {
        tokio::task::yield_now().await;
        assert_eq!(payload.bytes.len(), 256);
        assert_eq!(payload.bytes[0], 0);
        assert_eq!(payload.bytes[255], 255);
    }

    #[test]
    async fn cloneable_test_b(payload: &PrecomputedPayload) {
        tokio::task::yield_now().await;
        assert_eq!(payload.bytes.len(), 256);
        let sum: u64 = payload.bytes.iter().map(|b| *b as u64).sum();
        let expected: u64 = (0u64..=255).sum();
        assert_eq!(sum, expected);
    }
}
