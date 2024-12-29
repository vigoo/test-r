# Per-test configuration

Some aspects of the test runner can be enforced on a per-test or per-suite basis using special **attributes**, instead of relying on command line options.

## Enforce sequential execution
Parallelism of the test runner is normally controlled by the `--test-threads` command line argument. It is possible to enforce **sequential execution** for all tests within a **test suite** by putting the `#[sequential]` attribute on the module representing the suite:

```rust
use test_r::{sequential, test};

#[sequential]
mod suite {
    #[test]
    fn test1() {
        assert!(true);
    }

    #[test]
    fn test2() {
        assert!(true);
    }
}
```

The rest of the tests in the crate will still be parallelized based on the `--test-threads` argument.

The `#[sequential]` attribute can only be used on _inline modules_ due to a limitation in the current stable Rust compiler.
For non-inline modules, you can use the `sequential_suite!` macro instead in the following way:

```rust
use test_r::sequential_suite};

mod suite;

sequential_suite!(suite);
```

## Always or never capture output

Two attributes can enforce capturing or not capturing the standard output and error of a test. Without these attributes, the runner will either capture (by default), or not (if the `--nocapture` command line argument is passed).

When the `#[always_capture]` attribute is used on a `#[test]`, the output will be captured even if the `--nocapture` argument is passed. Conversely, the `#[never_capture]` attribute will prevent capturing the output even if the `--nocapture` argument is not passed.

## Timeout

The `#[timeout(duration)]` attribute can be used to enforce a timeout for a test. The timeout is specified in milliseconds as a number:

```rust
use test_r::{test, timeout};

#[timeout(1000)]
#[test]
async fn test1() {
    tokio::time::sleep(std::time::Duration::from_secs(2));
    assert!(true);
}
```

This feature only works when using the async test runner (enabled by the `tokio` feature).

## Reporting / ensuring time per test

There are command line arguments to enable _reporting_ test run times and _ensuring_ that each test runs within a certain time limit. 
The command line arguments enable these features for all tests. It is possible to individually configure this behavior per test using
the following attributes:

- `#[always_report_time]` will report the time taken by the test even if the `--report-time` argument is not passed.
- `#[never_report_time]` will prevent reporting the time taken by the test even if the `--report-time` argument is passed.
- `#[always_ensure_time]` will ensure that the test runs within the specified duration even if the `--ensure-time` argument is not passed.
- `#[never_ensure_time]` will ignore the `--ensure-time` argument for this test

Note that for ensuring time, it is not possible to overwrite the global time limit set using environment variables, which is the way the built-in Rust test runner works. For better control, use the `#[timeout(duration)]` attribute instead.
