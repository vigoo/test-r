# Getting started with `test-r`

`test-r` is a testing framework for Rust which is _almost_ a drop-in replacement for built-in tests, but enables several advanced features such as dependency injection, dynamic test generation, custom tags, inline customization of the test execution and more.

By replicating the built-in test framework's command line interface, `test-r` tests work seamlessly with IDEs like Visual Studio Code, IntelliJ IDEA, Zed, and others. `test-r` also implements many unstable features of the built-in test framework, such as customizable test output, reporting and ensuring execution time, shuffling test execution and running `#[bench]` benchmarks.

To start using `test-r`, add it to the `dev-dependencies` section of your `Cargo.toml`:

```toml
[dev-dependencies]
test-r = "1"
```

There are three additional steps to take when using `test-r` in place of the built-in tests:

1. Disabling the built-in test harness for every build target where `test-r` will be used
2. Enabling the `test-r` test harness by including its main function in every build target
3. Import `test-r`'s custom `test` attribute where `#[test]` is used

This is explained in details on the [Defining tests](./core_features/defining_tests.md) page, but the example below demonstrates how to set up a simple crate to run tests with `test-r`.

## Example

The following `Cargo.toml` file sets up a simple library crate with `test-r`:

```toml
[package]
name = "test-r-demo"
version = "0.1.0"
edition = "2024"

[lib]
harness = false # Disable the built-in test harness

[dev-dependencies]
test-r = "1"
```

And a simple `src/lib.rs` file defining a single public function and a test for it:

```rust
#[cfg(test)]
test_r::enable!(); // Enabling test-r's test harness (once per build target)

pub fn lib_function() -> u64 {
    println!("lib_function called");
    11
}

#[cfg(test)]
mod tests {
    use test_r::test; // Replacing the built-in #[test] attribute

    use super::*;

    #[test]
    fn test_lib_function() {
        assert_eq!(lib_function(), 11);
    }
}
```

## Optional crate features
The `test-r` test framework with the default set of enabled features supports running both sync and async tests, using [Tokio](https://tokio.rs) as the async runtime.
It is possible to turn off the async support by disabling the `tokio` feature:

```toml
[dev-dependencies]
test-r = { version = "1", default-features = false }
```

## Real-world usage
This section lists known projects that use `test-r`:

- [Golem Cloud](https://golem.cloud) uses `test-r` for all its unit and integration tests. ([GitHub](https://github.com/golemcloud/))

## What is not supported?

The following features are not supported by `test-r`:

- Running **doctests**
- Output capturing cannot be used together with parallel execution AND dependency injection. Any two of these three features can be chosen, but not all three at the same time.

## Acknowledgements

Most of `test-r`'s features were inspired by working with test frameworks in other languages, especially the [ZIO Test](https://zio.dev/reference/test/) framework for Scala. The idea of replicating the built-in harness' command line interface came from the [libtest-mimic crate](https://github.com/LukasKalbertodt/libtest-mimic/). For some features that replicate built-in functionality, parts of the original [libtest source code](https://github.com/rust-lang/rust/tree/master/library/test) have been reused.
