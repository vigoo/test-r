# Working with flaky tests

Tests can be sometimes flaky, and only fail sporadically or depending on the environment or hardware they run on.

`test-r` provides two ways to handle flaky tests:

## Marking tests as known to be flaky
By using the `#[flaky(n)]` attribute, where `n` is a number, we acknowledge that a test is known to be flaky. The test runner allows up to `n` total attempts (the initial execution plus at most `n - 1` retries), stopping at the first successful attempt.

Both synchronous and asynchronous tests retry test-body panics and returned `Result::Err` values. Dynamically registered tests using `FlakinessControl::RetryKnownFlaky(n)` follow the same rules. Reported retry counts reflect additional executions, not the configured attempt budget.

Harness failures, including asynchronous timeouts and critical run-time threshold violations, are not retried. An asynchronous timeout applies to each attempt separately, not to the entire retry sequence. Dependency-construction failures and worker crashes are not retried by this attribute either.

`should_panic` is evaluated after the retry loop; this attribute does not change the existing expected-panic behavior.

```rust
use test_r::{flaky, test};

#[flaky(3)]
#[test]
fn flaky_test() {
    assert!(false); // This test will fail 3 times before being marked as failed
}
```

## Ensuring tests are not flaky

The opposite appraoch is to ensure that a test is not flaky by running it multiple times. This can help in diagnosing flakiness and reproducing issues locally. The `#[non_flaky(n)]` attribute will run a test `n` times before marking it as succeeded.

```rust
use test_r::{non_flaky, test};

#[non_flaky(3)]
#[test]
fn non_flaky_test() {
    assert!(true); // This test will pass 3 times before being marked as succeeded
}
```
