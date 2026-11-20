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

Multiple filter strings can be passed after `--`, which will run all tests matching **any** of the filters:

```sh
cargo test -- hello world
```

This runs all tests whose name contains `hello` **or** `world`.

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

The `--skip` option can be used to skip tests whose names contain the given substring (just like if they were marked with `#[ignore]`). It can be used multiple times to skip multiple groups of tests. When `--exact` is also passed, `--skip` requires an exact name match instead of a substring match.

```sh
cargo test -- --skip slow
```

This skips all tests whose name contains `slow`. Tags can also be used with `--skip`:

```sh
cargo test -- --skip ':tag:expensive'
```

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

Listing applies the same name, tag, and skip filters as running tests. For example, to verify which tests have a tag without running them:

```sh
cargo test ':tag:expensive' -- --list
```

## Test output
There are various options controlling the **output** of the test runner. See the [test output chapter](/core_features/test_output.md) for more details.

## Debugging
Output capturing is implemented by forking one or more child processes and attaching to their standard output and error channels. This means that attaching a **debugger** to the parent process will not work as expected. When using a debugger, always pass the `--nocapture` flag to the test runner to disable output capturing, which guarantees that all the tests are executed in the single root process. 






## Running prebuilt test binaries with `cargo-test-r`
`cargo test-r run` accepts the same arguments as `cargo test`. With `--binaries-metadata <PATH>` it runs test binaries that were already built instead of invoking Cargo. `PATH` is the JSON written by:

```
cargo nextest list --list-type binaries-only --message-format json [--cargo-profile <PROFILE>] > binaries.json
```

The binaries run from the absolute paths recorded in that file, in their package's directory. Test name filters and arguments after `--` are passed to each binary:

```
cargo test-r run --binaries-metadata binaries.json -p my-crate --test integration -- --retry-failed 2
```

### Selecting binaries without running tests

`cargo test-r list` reads existing metadata, validates the selected artifacts and writes only selection JSON to stdout. It never runs Cargo or a test binary:

```sh
cargo test-r list --binaries-metadata binaries.json -p my-crate --test integration > selection.json
cargo test-r run --selection selection.json -- --retry-failed 2
```

Both `list` and `run --binaries-metadata` support these filters:

- `-p`/`--package` and `--exclude` match **Cargo package names**, not directory names. Both accept globs and can be repeated. Excludes apply even without `-p`.
- `--lib` selects library (including proc-macro) unit tests; `--bins` selects binary unit tests.
- Repeatable `--test <NAME>` selects integration test targets by name, with globs. `--tests` selects library, binary and integration test targets.
- Target-kind/name filters are additive: `--lib --test integration` selects both. Package filters and repeatable exact `--binary-id <ID>` filters further restrict the result.
- With no target filters, all entries in the metadata are eligible.

Repeat `--binaries-metadata <PATH>` to combine metadata files. Identical entries for the same binary id are merged. Conflicting entries (including different executable paths or build variants) fail with both candidates in the diagnostic, rather than picking one.

### Selection JSON version 1

The top-level fields, in output order, are `format`, `version`, `inputs`, `binaries`:

```json
{
  "format": "cargo-test-r-selection",
  "version": 1,
  "inputs": [{ "path": "binaries.json", "sha256": "<64 lowercase hex digits>" }],
  "binaries": [{
    "binary-id": "my-crate::integration",
    "package-id": "path+file:///w/my-crate#my-crate@0.1.0",
    "package-name": "my-crate",
    "kind": "test",
    "name": "integration",
    "build-variant": "3b1e0c9a77d2",
    "executable": "/w/target/debug/deps/integration-4f1a2b",
    "working-directory": "/w/my-crate"
  }]
}
```

Inputs remain in command-line order; each hash is SHA-256 of the exact metadata bytes read. Binaries are sorted by `binary-id` in byte order, which is also their run order. Binary ids use nextest's stable scheme: `pkg` for libraries, `pkg::name` for integration tests, and `pkg::bin/name` for binary unit tests. Each binary has the fields shown above in that order. Paths to executables and working directories are absolute. Package names come from `#name@version` in the Cargo package id, or the last path component for older `…/name#version` ids.

`build-variant` is the first 12 lowercase hex digits of SHA-256 over compact JSON containing, in order, the original target directory, nextest's `platforms`, legacy `target-platforms` and `target-platform`, sorted base output directories (profiles), and the binary's host/target build platform. It identifies the build metadata, not the executable contents. Nextest binaries-only metadata does not enumerate feature sets; different executables for one id are rejected even when these build metadata fields agree.

`run --selection` validates the format/version and all artifacts before starting anything. It runs exactly the saved binaries, in binary-id order, without reopening input metadata or invoking Cargo. Package/target filters, `--target-dir` and `--binaries-metadata` cannot be combined with `--selection`; make a new selection instead. Test name filters and arguments after `--` are still forwarded to every selected binary. A selection does not freeze executable contents: rebuilding can replace them.

### Extracted nextest archives and errors

After `cargo test-r reuse-nextest-archive --archive-file tests.tar.zst`, both `list` and `run` automatically read `target/nextest-archive/target/nextest/binaries-metadata.json` when no explicit input is given. `--target-dir <DIR>` changes the archive location to `<DIR>/nextest-archive`. Binary paths are relocated relative to the original target directory; package working directories are relocated from the original `workspace_root` in the archive's `target/nextest/cargo-metadata.json` to the current workspace. This also supports archives built with redirected Cargo target directories.

Explicit metadata/selection input and `list` never fall back to Cargo. Unreadable or malformed metadata, unsupported package ids that cannot map to local directories, missing or non-executable selected artifacts, missing working directories and empty selections fail with exit code 1 and diagnostics on stderr, without selection output on stdout. On Unix, executable permission bits are checked; on Windows, files must have an `.exe` extension. These checks do not inspect executable contents or guarantee that the OS can load them; launch errors are reported by `run`. Build options such as `--profile` or `--features` are rejected in prebuilt mode. The run loop remains sequential and stops at the first failing binary; prebuilt `--no-fail-fast` and `--no-run` are not supported yet. Without explicit input or an extracted archive, `run` still forwards to `cargo test`.
