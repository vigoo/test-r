//! Worker-process metadata exposed to user-defined dependency factories.
//!
//! The test runner spawns one worker subprocess per test thread when output
//! capturing is on (see [`crate::sync`]/[`crate::tokio`]). Each spawned
//! worker is assigned a zero-based **worker index** by the parent. `PerWorker`
//! dependency constructors can read that index via [`worker_index`] to seed
//! per-worker state (for example, a unique-id counter that must not collide
//! with other workers).
//!
//! When the test harness runs without spawning workers (the parent process
//! itself runs every test, e.g. under `--nocapture`, when no `Shared` deps
//! force serial execution, or when nothing requires capture in the first
//! place), [`worker_index`] returns `0`.

use std::sync::OnceLock;

static WORKER_INDEX: OnceLock<usize> = OnceLock::new();

/// Sets the worker index for the current process.
///
/// Called by the test runner entry points (`test_runner_sync` /
/// `test_runner_tokio`) at startup when the parent passed
/// `--worker-index <N>` on the command line. May be called at most once per
/// process; subsequent calls are silently ignored so the first observed value
/// wins (this matches `OnceLock` semantics and protects against accidental
/// re-initialisation from multiple test harnesses linked into the same
/// binary).
///
/// Crate-private: user code must not call this. It is only meaningful when
/// invoked by the test runner before any `PerWorker` constructor runs.
pub(crate) fn set_worker_index(idx: usize) {
    // Ignore double-init: keep the first observed value (matches OnceLock).
    let _ = WORKER_INDEX.set(idx);
}

/// Returns the zero-based worker index assigned to this OS process.
///
/// This is a **process-level** identifier, not a per-test-thread identifier.
/// Each spawned worker subprocess has at most one index; the top-level
/// parent and any "no spawn workers" execution path (e.g. `--nocapture`,
/// no captured-output requirement, or `Shared` deps forcing single-thread
/// execution) observes `0`.
///
/// # When this is useful
///
/// Only `PerWorker` constructors and the tests they feed see meaningful
/// per-worker values, because they are the only constructors that run
/// inside spawned worker subprocesses. Parent-only scopes — `Shared`,
/// `Cloneable`, `Hosted`, and `HostedRpc` — always run in the top-level
/// parent and therefore always observe `0`. Do not use `worker_index()`
/// to partition state inside those constructors; the answer is always 0.
///
/// # Use with `PerWorker` dependencies
///
/// Combine with `#[test_dep(scope = PerWorker)]` to seed per-worker state.
/// For example, a unique-id counter that partitions its id space by worker
/// so that two parallel workers cannot mint the same id:
///
/// ```ignore
/// use std::sync::atomic::AtomicU16;
/// use test_r::test_dep;
///
/// pub struct LastUniqueId {
///     pub id: AtomicU16,
/// }
///
/// #[test_dep(scope = PerWorker)]
/// fn last_unique_id() -> LastUniqueId {
///     // Reserve the high 8 bits for the worker index, leaving 8 bits per
///     // worker for the local sequence. Adjust the shift to whatever fits
///     // the underlying integer width.
///     LastUniqueId {
///         id: AtomicU16::new((test_r::worker_index() as u16) << 8),
///     }
/// }
/// ```
pub fn worker_index() -> usize {
    *WORKER_INDEX.get().unwrap_or(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_zero_when_unset() {
        // This test runs in the test-r-core unit test binary, where no
        // worker subprocess is in play. `worker_index` must default to 0
        // even when `set_worker_index` was never called.
        //
        // Note: because `WORKER_INDEX` is a process-global OnceLock,
        // calling `set_worker_index` in another test would poison this
        // observation. We deliberately do not call the setter from any
        // unit test in this module.
        assert_eq!(worker_index(), 0);
    }
}
