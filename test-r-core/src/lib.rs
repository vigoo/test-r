pub mod args;
pub mod internal;

#[allow(dead_code)]
mod sync;

#[cfg(not(feature = "tokio"))]
pub use sync::test_runner;

mod execution;
mod output;
#[cfg(feature = "tokio")]
mod tokio;

#[cfg(feature = "tokio")]
pub use tokio::test_runner;
