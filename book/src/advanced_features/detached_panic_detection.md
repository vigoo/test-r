# Detached panic detection

When a test spawns threads or async tasks that outlive the test body, panics in those detached contexts are normally invisible — the test passes while the panic message is silently printed to stderr. **test-r** can detect these panics and fail the test, but only when using test-r's own spawn functions.

## Spawn functions

test-r provides two spawn functions that propagate test context and capture panics:

- `test_r::spawn_thread` — wraps `std::thread::spawn`
- `test_r::spawn` — wraps `tokio::spawn` (requires the `tokio` feature)

These are drop-in replacements with the same signatures:

```rust
use test_r::{test, spawn_thread};

#[test]
fn test_with_background_thread() {
    let handle = spawn_thread(|| {
        // If this panics, the test will fail
        assert_eq!(2 + 2, 4);
    });
    handle.join().unwrap();
}
```

```rust
use test_r::{test, spawn};

#[test]
async fn test_with_spawned_task() {
    let handle = spawn(async {
        // If this panics, the test will fail
        assert_eq!(2 + 2, 4);
    });
    handle.await.unwrap();
}
```

## How it works

By default, every test uses `DetachedPanicPolicy::FailTest`. When you use `spawn_thread` or `spawn`:

1. The current test's identity is propagated to the new thread or task.
2. The closure is wrapped in `catch_unwind` to intercept any panic.
3. If a panic occurs, it is recorded and associated with the originating test.
4. After the test body completes, the runner checks for collected panics and fails the test if any were found.

> **Important:** If you use `std::thread::spawn` or `tokio::spawn` directly, panics in those threads/tasks will **not** be detected by test-r. They will be printed to stderr by the default panic hook but will not cause the test to fail.

## Opting out

If a test intentionally spawns work that may panic, you can disable detection with the `#[ignore_detached_panics]` attribute:

```rust
use test_r::{test, ignore_detached_panics, spawn_thread};

#[ignore_detached_panics]
#[test]
fn test_that_expects_detached_panics() {
    spawn_thread(|| {
        panic!("this is expected");
    });
}
```
