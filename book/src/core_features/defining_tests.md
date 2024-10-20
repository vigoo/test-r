# Defining tests

## Enabling the test-r harness
Writing tests with `test-r` is very similar to writing tests with the built-in test framework, but there are a few differences.

### Disabling the built-in test harness
First, for every build target where `test-r` is going to be used, the built-in test harness must be disabled.

This is done by putting `harness = false` in build target's section in `Cargo.toml`:

```toml
[lib]
harness = false

[[bin]]
harness = false

[[test]]
name = "integ-test-1"
harness = false

[[test]]
name = "integ-test-2"
harness = false

# ...
```

### Mixing test-r and the built-in test harness
It is recommended to turn off running tests completely in the rest of the targets. For example if the crate produces both a library and an executable, and all the tests are in the library part, then put `test = false` in the `[[bin]]` section:

```toml
[[bin]]
test = false

[lib]
harness = false
```

Without this, `cargo test` will run all the test harnesses including the one where the built-in harness is not disabled (`[[bin]]` in this case), which may fail on some unsupported command line arguments that the `test-r` harness accepts. 

If the intention is to use both `test-r` and the built-in test harness in the same crate, that's possible, but be careful with the command line arguments passed to `cargo test` as some of them may be only supported by the _unstable_ version of the built-in test framework.

### Enabling the test-r harness
For every target where the built-in harness was disabled (with `harness = false`), we need to install `test-r`'s test runner instead. In other words, if the compilation is in `test` mode, we have to define a `main` function that runs the `test-r` test runner.

This can be done by adding the following macro invocation at the root of the given build target:

```rust
#[cfg(test)]
test_r::enable!();
```

- For `[lib]` targets, this should be in `src/lib.rs` (or whatever crate root is specified)
- For `[[bin]]` targets, this should be in the `src/main.rs`, `src/bin/*.rs` files or the one explicitly set in the crate manifest, for each binary
- For `[[test]]` targets, this should be in the `tests/*.rs` files for each test

## Writing tests
Writing tests is done exactly the same way as with the built-in test framework, but with using `test-r`'s `#[test]` attribute instead of the built-in one. We recommend importing the test attribute with `use test_r::test;` so the actual test definitions look identical to the built-in ones, but it is not mandatory.

```rust
#[cfg(test)]
mod tests {
    use test_r::test;

    #[test]
    fn test_lib_function() {
        assert_eq!(lib_function(), 11);
    }
}
```

Within the test function itself any assertion macros from the standard library or any of the third-party assertion crates can be used. (All panics are caught and reported as test failures.)

## Writing async tests

The same `#[test]` attribute can be used for async tests as well. The test runner will automatically detect if the test function is async and run it accordingly.

```rust
#[cfg(test)]
mod tests {
    use test_r::test;

    #[test]
    async fn test_async_function() {
        assert_eq!(async_lib_function().await, 11);
    }
}
```

Support for async tests requires the `tokio` feature, which is enabled by default.

<div class="warning">
There is a difference in how <code>test-r</code> runs async tests compared to how <code>#[tokio::test]</code> does. While tokio's test attribute spawns a new current-thread (by default) Tokio runtime for each test, <code>test-r</code> uses a single multi-threaded runtime to run all the tests. This is intentional, to allow <b>shared dependencies</b> that in some way depend on the runtime itself. 
</div>

## Tests returning Result
Tests in `test-r` can have a `Result<_, _>` return type. This makes it easier to chain multiple functions within the test that can return with an `Err`, no need to `unwrap` each. A test that returns a `Result::Err` will be marked as failed just like as if it had panicked.

```rust 
#[cfg(test)]
mod tests {
    use test_r::test;

    #[test]
    fn test_lib_function() -> Result<(), Box<dyn std::error::Error>> {
        let result = lib_function()?;
        assert_eq!(result, 11);
        Ok(())
    }
}
```

## Ignoring tests
The standard `#[ignore]` attribute can be used to mark a test as ignored.

```rust
#[test]
#[ignore]
fn ignored_test() {
    assert!(false);
}
```

Ignored tests can be run with the `--include-ignored` or `--ignored` flags, as explained in the [running tests page](running_tests.md).

## Testing for panics
The `#[should_panic]` attribute can be used to mark a test as expected to panic. The test will pass if it panics, and fail if it doesn't.

```rust
#[test]
#[should_panic]
fn panicking_test() {
    panic!("This test is expected to panic");
}
```

Optionally the `expected` argument can be used to only accept panics containing a specific message:

```rust
#[test]
#[should_panic(expected = "expected to panic")]
fn panicking_test() {
    panic!("This test is expected to panic");
}
```

