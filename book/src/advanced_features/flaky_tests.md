# Working with flaky tests

Tests can be sometimes flaky, and only fail sporadically or depending on the environment or hardware they run on.

`test-r` provides two ways to handle flaky tests:

## Marking tests as known to be flaky
By using the `#[flaky(n)]` attribute, where `n` is a number, we acknowledge that a test is known to be flaky, and the test runner will retry it up to `n` times before marking it as failed.

```rust
use test_r::{flaky, test};

#[flaky(3)]
#[test]
fn flaky_test() {
    assert!(false); // This test will fail 3 times before being marked as failed
}
```

### Retry reporting

Retry notifications are reported in both direct and captured worker execution,
including parallel runs. `--show-output` only controls captured log visibility;
it does not change retry accounting.

In CTRF reports, `retries` counts performed repetitions after the first attempt,
not the configured budget. A known-flaky test that passes immediately has no
`retries` or `flaky` field. Once retried, it has `flaky: true`, even if all
attempts fail: this indicates that a known-flaky test was retried, not that it
eventually recovered. With `#[flaky(3)]`, a persistent failure makes three
attempts and reports `retries: 2` while remaining failed.

Worker retry notifications are delivered with the final test result, so plain
output displays them at completion rather than while an attempt is running.

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

CTRF also counts these verification repetitions in `retries`, but records
`flaky: false` for them.
