# Tracing

Subscribers for [Tokio tracing](https://github.com/tokio-rs/tracing) usually need to be set up once at the beginning of the application, and further calls to their initialization functions may cause panics. 

With `test-r`, the [shared dependency feature](../advanced_features/dependency_injection.md) can be used to set up the tracing subscriber once before the first test is executed, and keep it alive until the end of the test run.

The following example demonstrates this using the `tracing-subscriber` crate:

```rust
use tracing_subscriber::fmt::format::FmtSpan;
use test_r::{test_dep, test};

struct Tracing;

impl Tracing {
    pub fn init() -> Self {
        tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer().pretty()
        ).init();
        Self
    }
}

#[test_dep]
fn tracing() -> Tracing {
    Tracing::init()
}

#[test]
fn test1(_tracing: &Tracing) {
    tracing::info!("test1");
}

#[test]
fn test2(_tracing: &Tracing) {
    tracing::info!("test2");
}
```
