# Test output

The default setting of `test-r` is to use the **pretty** format and capture test outputs.  

## Output format
There are four supported output formats in `test-r`, which can be selected with the `--format` flag:

- `pretty` (default) - human-readable output showing the progress and the final results in a verbose way
- `terse` - human-readable output, only emitting a single character for each test during the test run
- `json` - emits JSON messages during the test run, useful for integration with other tools like IDEs
- `junit` - writes a JUnit XML test report, useful for generating browsable test reports 

When using the `pretty` (default) mode, the `--color` flag can be used to control whether the output should use colors or not:

- `auto` (default) - colors are used if the terminal supports them
- `always` - always use colors
- `never` - do not use colors

## Capturing the test output
When **output capturing** is enabled, lines written to either the standard output or standard error channels are not shown immediately as the test runs. Instead, they are only shown if the test fails. This allows nicer visual tracking of the test progress and results.

The following options control this behavior:

- `--nocapture` - disables output capturing, showing the output of each test as it runs
- `--show-output` - shows the output of all tests **after they finish**, regardless of whether they passed or failed

Note that this global setting of output capturing can be overwritten on a per-test basis using the `#[always_capture]` and `#[never_capture]` attributes, as explained in the [per-test configuration chapter](/advanced_features/per_test_configuration.md). 

### Host-side output capture

Some test dependencies — most notably `#[test_dep(scope = HostedRpc, …)]` owners (see [Dependency sharing strategies](/advanced_features/dependency_sharing.md)) — run **in the parent test runner process**, not in the worker subprocesses that own a given test. Anything those owners write to standard output or standard error (including from background threads they spawn, or from inside `dispatch`) used to be invisible per-test: it landed on the runner's own stdout/stderr, which is either swallowed by `cargo test` or, worse, interleaved into the structured `--format=json` / `junit` / `ctrf` streams.

When output capturing is on (the default), `test-r` now also captures the parent's own stdout/stderr in the background and attributes each line to the test(s) that were running when the line was produced — a best-effort, window-overlap based attribution. These records show up alongside the test's own captured output, prefixed with `[host] ` so the provenance is visible. In `--format=pretty`, the prefix is also rendered in a dimmed colour.

```text
---- mycrate::my_module::tests::my_test stdout/err ----
my own println from inside the test
[host] HOST_DISPATCH_HIT
[host] HOST_BG_THREAD_TICK
```

This feature is automatic, supported on Unix and Windows, and disabled in `--nocapture` mode (where everything just goes to the terminal anyway). If a line cannot be attributed to any test (e.g. it was produced before the suite started or in a gap between tests) it is silently dropped.

When tests run in parallel and their windows overlap, a single host-side line may legitimately be attributed to several tests at once.

Host-side attribution happens once at suite end, so the host lines only appear in formatters that render per-test output **after** the suite finishes: `pretty`, `junit` and `ctrf`. The `json` formatter is a streaming format that emits a `test` event for each test as soon as it finishes — well before suite-end attribution — so its per-test `stdout` field does not include host-side lines. The `terse` formatter never reports per-test output regardless of `--show-output`, so host lines do not appear there either.

<div class="warning">
When attaching a debugger, always pass the `--nocapture` flag to the test runner to disable output capturing, which guarantees that all the tests are executed in the single root process the debugger is attached to.
</div>

<div class="warning">
Output capturing, parallel execution and shared test dependencies cannot be used together. The reason is that output capturing relies on forking child processes to capture their outputs, and the shared dependencies cannot be shared between these processes. If shared dependencies are used, and the <code>--nocapture</code> flag is not present, the test runner will emit a warning and fall back to single threaded execution. 
</div>

## Measuring and ensuring execution time
By default `test-r` follows the built-in test harness behavior and does not report test execution times. This can be changed by passing the `--report-time` flag. The `--ensure-time` flag not only reports these per-test execution times, but fails the test run if they exceed a pre-configured value. Learn more about this in [The Rust Unstable Book](https://doc.rust-lang.org/beta/unstable-book/compiler-flags/report-time.html).

Note that `test-r` provides a nicer way to fail long running tests (but only if the `tokio` feature is enabled) using the `#[timeout(ms)]` attribute, as explained in the [per-test configuration chapter](/advanced_features/per_test_configuration.md).

## Saving the output to a log file
The test output can be saved into a log file using the `--logfile <path>` flag. Because of the [issue described in the Rust issue tracker](https://github.com/rust-lang/rust/issues/105424), the test runner cannot directly use the provided path as other test harnesses would overwrite it. Instead, `test-r` interprets the provided path as a template, and appends a random UUID to its file name part for each generated log file. This allows saving multiple JUnit test reports, for example, into a single directory, where a test browser can pick them up from.

