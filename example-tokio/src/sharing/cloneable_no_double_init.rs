//! Regression fixture for the `--nocapture` (no-spawn-workers) double-init
//! bug on the tokio runner.
//!
//! In `--nocapture` mode, `spawn_workers` is `false` and the runner runs
//! every test in the parent process. The parent still calls
//! `collect_parent_shared_dependencies_async` to compute Cloneable wire
//! bytes, but historically those wire bytes were then thrown away — so
//! `materialize_deps` later re-ran the Cloneable constructor, causing it to
//! run twice end-to-end.
//!
//! This fixture prints a unique marker line every time its Cloneable
//! constructor runs. The companion integration test in
//! `test-r/tests/tests.rs` runs this binary with `--nocapture` and asserts
//! that the marker appears exactly once on stdout.

#[cfg(test)]
mod tests {
    use test_r::core::CloneableDep;
    use test_r::{test, test_dep};

    #[derive(Debug)]
    pub struct DoubleInitProbe;

    impl CloneableDep for DoubleInitProbe {
        fn to_wire(&self) -> Vec<u8> {
            Vec::new()
        }

        fn from_wire(_bytes: &[u8]) -> Self {
            DoubleInitProbe
        }
    }

    /// Marker is grep-counted by the companion integration test. Must be
    /// printed unconditionally and on a single line so the count is stable.
    #[test_dep(scope = Cloneable)]
    async fn build_probe() -> DoubleInitProbe {
        println!("CLONEABLE_NO_DOUBLE_INIT_MARKER: build_probe()");
        DoubleInitProbe
    }

    /// The test body itself is empty — the assertion is performed by the
    /// integration test that counts marker occurrences on stdout. We only
    /// need to depend on the Cloneable so the runner actually materialises
    /// it.
    #[test]
    async fn cloneable_no_double_init_test(_probe: &DoubleInitProbe) {}
}
