//! Examples for the per-dependency sharing strategies supported by
//! test-r, exercised under the tokio async runtime: `PerWorker`,
//! `Cloneable`, `Hosted` (with `worker = descriptor`,
//! `worker = rpc(Trait)`, and `worker = both(Trait)`, including
//! async-only descriptor reconstruction via `AsyncHostedDep`), and
//! the legacy `HostedRpc` scope.
//!
//! Each sub-module demonstrates one scope (or one variant of the
//! Hosted worker-view picker) under the tokio runner.

pub mod cloneable_basic;
pub mod hosted_async_worker;
pub mod hosted_async_worker_rpc_legacy;
pub mod hosted_basic;
pub mod hosted_both_async_ctor;
pub mod hosted_both_async_ctor_stub_only;
pub mod hosted_both_async_descriptor;
pub mod hosted_both_basic;
pub mod hosted_rpc_basic;
pub mod hosted_rpc_macro;
pub mod hosted_rpc_macro_async;
pub mod per_worker_basic;
