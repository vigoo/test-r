//! Examples for the per-dependency sharing strategies supported by
//! test-r: `PerWorker`, `Cloneable`, `Hosted` (with `worker = descriptor`,
//! `worker = rpc(Trait)`, and `worker = both(Trait)`) and the legacy
//! `HostedRpc` scope.
//!
//! Each sub-module demonstrates one scope (or one variant of the Hosted
//! worker-view picker) and is also exercised by the integration tests in
//! [`tests/sharing.rs`](../../tests/sharing.rs).

pub mod cloneable_basic;
pub mod hosted_basic;
pub mod hosted_both_basic;
pub mod hosted_rpc_basic;
pub mod hosted_rpc_macro;
pub mod per_worker_basic;
pub mod per_worker_index;
