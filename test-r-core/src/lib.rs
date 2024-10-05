pub mod args;
pub mod bench;
mod execution;
pub mod internal;
mod ipc;
mod output;
mod stats;
#[cfg(feature = "tokio")]
mod tokio;

#[allow(dead_code)]
mod sync;

#[cfg(not(feature = "tokio"))]
pub use sync::test_runner;

#[cfg(feature = "tokio")]
pub use tokio::test_runner;
