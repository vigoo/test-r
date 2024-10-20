# Benches

`test-r` provides a simple benchmark runner as well, very similar to the built-in one in unstable Rust. The main differences are that `test-r` allows defining async bench functions too (when the `tokio` feature is enabled), and that benchmark functions also support [dependency injection](./dependency_injection.md).

## Defining benchmarks

To define a benchmark, just use the `#[bench]` attribute instead of a `#[test]` attribute on a function that takes a mutable reference to a `Bencher`: 

```rust
use test_r::{bench, Bencher};

#[bench]
fn bench1(b: &mut Bencher) {
    b.iter(|| 10 + 11);
}
```

The benchmark framework will measure the performance of the function passed to the `iter` method on the bencher.

If a benchmark needs **[shared dependencies](./dependency_injection.md)**, they can be added as additional parameters to the benchmark function. The `&mut Bencher` parameter must always be the first one.

```rust
use test_r::{bench, Bencher};

struct SharedDependency {
    value: i32,
}

#[bench]
fn bench2(b: &mut Bencher, shared: &SharedDependency) {
    b.iter(|| shared.value + 11);
}
``` 

### Async benchmarks
When the `tokio` feature is enabled, benchmarks can be async too. Just use the `#[bench]` attribute on an async function that takes a mutable reference to an `AsyncBencher`:

```rust
use test_r::{bench, AsyncBencher};

#[bench]
async fn bench1(b: &mut AsyncBencher) {
    b.iter(|| Box::pin(async { 10 + 11 })).await;
}
```

## Running benchmarks
Benchmarks are run by default as part of `cargo test`, but they can be also separately executed using `cargo bench`, or by passing the `--bench` flag to `cargo test`.
