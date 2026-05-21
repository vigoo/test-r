//! Examples for the per-dependency sharing strategies introduced in Phases
//! 1A (`PerWorker`, `Cloneable`), 1B (`Hosted`) and 1C (`HostedRpc`).
//!
//! Each sub-module demonstrates one scope and is also exercised by the
//! integration tests in [`tests/sharing.rs`](../../tests/sharing.rs).

pub mod cloneable_basic;
pub mod hosted_basic;
pub mod hosted_rpc_basic;
pub mod per_worker_basic;
