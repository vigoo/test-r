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

## Retrying failed tests from the command line

`--retry-failed <COUNT>` is an opt-in run-level policy, for example for CI. After a
run with failures, the runner reruns only the failed tests, at most `COUNT` more
times, and stops as soon as none fail. Tests that passed keep their result and are
not run again. Each rerun is a new suite attempt, so the dependencies of the rerun
tests are constructed again, and in capture mode the tests run in new worker
processes.

A rerun happens only when every failure is a test panic or returned error of a test
without its own flakiness control. Otherwise the run stops, stays failed, and prints
why. Such failures are never retried, and also block the retry of every other
failure in the same run:

- harness failures: asynchronous timeouts, `--ensure-time` critical time violations,
  `should_panic` tests that did not panic, and other harness errors;
- failures of `#[flaky]` tests, which already used their own retry budget, and of
  `#[non_flaky]` tests, whose repetitions must all pass.

Dependency construction failures, worker crashes and interrupted runs end the
process without a complete result, so they are not retried either.

`--retry-failed` and `--flaky-run` cannot be combined, so a run has one budget of
suite attempts. Without `--retry-failed` the runner behaves as before.

In CTRF reports a recovered test has `flaky: true`, `retries` counts its reruns, and
`extra.testR.attempts` lists every invocation, including the initial failure. See
[CTRF retry evidence](../core_features/test_output.md#ctrf-retry-evidence).

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
