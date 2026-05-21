//! Examples for the per-dependency sharing strategies introduced in Phases
//! 1A (`PerWorker`, `Cloneable`) and 1B (`Hosted`), exercised under the
//! tokio async runtime.

pub mod cloneable_basic;
pub mod hosted_basic;
pub mod per_worker_basic;
