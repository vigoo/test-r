//! Example: a `Cloneable` `#[test_dep]`.
//!
//! With `scope = Cloneable` the parent runs the constructor exactly once, then
//! ships the wire bytes returned by [`CloneableDep::to_wire`] to every worker
//! child. Each worker calls [`CloneableDep::from_wire`] to reconstruct a
//! local instance.

#[cfg(test)]
mod tests {
    use test_r::core::CloneableDep;
    use test_r::{test, test_dep};

    /// A simple `Cloneable` dep that carries a payload by value.
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

    #[test_dep(scope = Cloneable)]
    fn create_payload() -> PrecomputedPayload {
        println!("CLONEABLE: building payload in parent");
        PrecomputedPayload {
            bytes: (0..=255u8).collect(),
        }
    }

    #[test]
    fn cloneable_test_a(payload: &PrecomputedPayload) {
        assert_eq!(payload.bytes.len(), 256);
        assert_eq!(payload.bytes[0], 0);
        assert_eq!(payload.bytes[255], 255);
    }

    #[test]
    fn cloneable_test_b(payload: &PrecomputedPayload) {
        assert_eq!(payload.bytes.len(), 256);
        let sum: u64 = payload.bytes.iter().map(|b| *b as u64).sum();
        let expected: u64 = (0u64..=255).sum();
        assert_eq!(sum, expected);
    }
}
