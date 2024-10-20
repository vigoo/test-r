# Running tests

`test-r` replicates the command line interface of the built-in test harness, so every integration (scripts, IDE support, etc) should work just like without using `test-r`.
This includes some of the unstable flags too, `test-r` let's use them without the need to enable the unstable features in the compiler. 

## Cargo test parameters vs test-r parameters
The `cargo test` command takes some of its own options, a test name, and a list of arguments passed to the test harness itself:```

```
Usage: cargo test [OPTIONS] [TESTNAME] [-- [ARGS]...]
```

The paramters passed in `OPTIONS` select which test targets to build and run. See [the official documentation](https://doc.rust-lang.org/cargo/commands/cargo-test.html) for more details.

`TESTNAME` is an optional parameter which selects which tests to run in each selected test target. How exactly it is interpreted depends on other options passed in the `ARGS` part.

## Choose what to run

### Matching on test names
```sh
cargo test hello
```

executes all tests that have the `hello` substring in their **fully qualified name** (module path + function name).

```sh
cargo test hello -- --exact
```

will only run the test that has the exact **fully qualified name** `hello`, which in this case means a function named `hello` in the root module.

There is a special syntax to match on **tags**, [see the tags chapter](/advanced_features/tags.md) for more details.

### Ignored tests

Tests marked with the `#[ignore]` attribute are not run by default. To run them, use the `--include-ignored` flag.
It is also possible to run **only the ignored tests** with the `--ignored` flag.

### Tests expecting panic

Tests using the `#[should_panic]` attribute are run by default, but can be skipped with the `--exclude-should-panic` flag.

### Tests vs benchmarks

The framework supports not only tests (defined with `#[test]`), but also benchmarks (defined with `#[bench]`). By default, the test runner executes both. It is possible to only run tests or benches with the `--test` and `--bench` flags.

### Skipping some tests

The `--skip` option can be used to skip some tests (just like if they were marked with `#[ignore]`). It can be used multiple times to mark multiple tests to skip. 

## Parallelism
By default, the test runner uses as many threads as there are logical cores on the machine. This can be changed with the `--test-threads` flag.

```
cargo test -- --test-threads=1
```

Note that parallelism can be also controlled on the code level **per test suite** with the `#[sequential]` attribute. See the [per-test configuration chapter](/advanced_features/per_test_configuration.md) for more details.

## Shuffle
The test runner executes tests in definition order. To shuffle the order, use the `--shuffle` flag. To have a deterministic, but shuffled order, use the `--shuffle-seed` providing a numeric seed.

## Listing tests
It is possible to just list all the available tests, without executing anything with the --list command:

```sh
cargo test -- --list
```

## Test output
There are various options controlling the **output** of the test runner. See the [test output chapter](/core_features/test_output.md) for more details.

## Debugging
Output capturing is implemented by forking one or more child processes and attaching to their standard output and error channels. This means that attaching a **debugger** to the parent process will not work as expected. When using a debugger, always pass the `--nocapture` flag to the test runner to disable output capturing, which guarantees that all the tests are executed in the single root process. 






