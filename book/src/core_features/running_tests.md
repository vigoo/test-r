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

Both `list` and prebuilt `run` support these filters:

- `-p`/`--package` and `--exclude` match **Cargo package names**, not directory names. Both accept globs and can be repeated. Excludes apply even without `-p`.
- `--lib` selects library (including proc-macro) unit tests; `--bins` selects binary unit tests.
- Repeatable `--test <NAME>` selects integration test targets by name, with globs. `--tests` selects library, binary and integration test targets.
- Target-kind/name filters are additive: `--lib --test integration` selects both. Package filters and repeatable exact `--binary-id <ID>` filters further restrict the result.
- With no target filters, all entries in the metadata are eligible.

Repeat `--binaries-metadata <PATH>` to combine metadata files. Identical entries for the same binary id are merged. Conflicting entries (including different executable paths or build variants) fail with both candidates in the diagnostic, rather than picking one.

### Reading Cargo build messages

Instead of nextest metadata, both `list` and `run` accept repeatable `--cargo-messages <PATH>` inputs. Build separately, capturing Cargo's JSON Lines stdout (not its human-readable stderr):

```sh
cargo test --no-run --message-format json --target-dir /ci/build-output > cargo-messages.jsonl
cargo test-r list --cargo-messages cargo-messages.jsonl --tests > selection.json
cargo test-r run --selection selection.json
# Or select and run directly:
cargo test-r run --cargo-messages cargo-messages.jsonl -p my-crate --test integration
```

`cargo build --tests --message-format json` output works too. Only `compiler-artifact` records with `profile.test == true` and a non-null `executable` are eligible. Other JSON messages are ignored; every line must be valid JSON, even if it is not an artifact or would be filtered out. Malformed lines fail with the input path and line number. An empty selection, missing artifact, or non-executable artifact fails before anything runs.

Identity comes exclusively from the package id and target kind/name, never filenames or modification times. Library and proc-macro unit tests use `pkg`, integration targets use `pkg::name`, and other executable targets use `pkg::kind/name` (for example `pkg::bin/tool`). Package names come from the package id; working directories come from the parent of the absolute `manifest_path`. Thus different packages with same-named targets remain distinct even when their source directories have the same name.

Cargo `build-variant` is the first 12 lowercase hex digits of SHA-256 over compact JSON `[profile, sorted-deduplicated-features, target_triple]`. The entire profile object is retained, with sorted object keys. Standard Cargo JSON does not report a target triple; that component is null unless a producer supplies the optional `target_triple` string. Different executable paths still conflict even if profile/features agree, so redirected or cross-target builds cannot silently replace each other. Repeated artifacts merge regardless of Cargo's `fresh` field. Feature/profile differences conflict even at the same executable path.

Cargo and nextest inputs may be mixed and use the same merge rules. Their variant metadata has different schemas, so overlapping binary ids generally conflict even if their executable paths agree; use disjoint selections or one source per binary. No input causes Cargo to be invoked by `list` or prebuilt `run`.

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

Inputs retain the order within each flag, with nextest inputs before Cargo-message inputs when mixed; each hash is SHA-256 of the exact input bytes read. Cargo-message inputs add `"kind": "cargo-messages"` after `sha256`; an omitted kind means nextest metadata, preserving existing version-1 selections. Binaries are sorted by `binary-id` in byte order, which is also their run order. Binary ids use nextest's stable scheme: `pkg` for libraries, `pkg::name` for integration tests, and `pkg::bin/name` for binary unit tests. Each binary has the fields shown above in that order. Paths to executables and working directories are absolute. Package names come from `#name@version` in the Cargo package id, or the last path component for older `…/name#version` ids.

`build-variant` is the first 12 lowercase hex digits of SHA-256 over compact JSON containing, in order, the original target directory, nextest's `platforms`, legacy `target-platforms` and `target-platform`, sorted base output directories (profiles), and the binary's host/target build platform. It identifies the build metadata, not the executable contents. Nextest binaries-only metadata does not enumerate feature sets; different executables for one id are rejected even when these build metadata fields agree.

`run --selection` validates the format/version and all artifacts before starting anything. It runs exactly the saved binaries, in binary-id order, without reopening input metadata or invoking Cargo. Package/target filters, `--target-dir`, `--binaries-metadata` and `--cargo-messages` cannot be combined with `--selection`; make a new selection instead. Test name filters and arguments after `--` are still forwarded to every selected binary. A selection does not freeze executable contents: rebuilding can replace them.

### Extracted nextest archives and errors

After `cargo test-r reuse-nextest-archive --archive-file tests.tar.zst`, both `list` and `run` automatically read `target/nextest-archive/target/nextest/binaries-metadata.json` when no explicit input is given. `--target-dir <DIR>` changes the archive location to `<DIR>/nextest-archive`. Binary paths are relocated relative to the original target directory; package working directories are relocated from the original `workspace_root` in the archive's `target/nextest/cargo-metadata.json` to the current workspace. This also supports archives built with redirected Cargo target directories.

Explicit metadata/selection input and `list` never fall back to Cargo. Unreadable or malformed metadata, unsupported package ids that cannot map to local directories, missing or non-executable selected artifacts, missing working directories and empty selections fail with exit code 1 and diagnostics on stderr, without selection output on stdout. On Unix, executable permission bits are checked; on Windows, files must have an `.exe` extension. These checks do not inspect executable contents or guarantee that the OS can load them; launch errors are reported by `run`. Build options such as `--profile` or `--features`, and `--no-run`, are rejected in prebuilt mode. Without explicit input or an extracted archive, `run` still forwards to `cargo test`, including `--no-fail-fast`.

### Continuing after a failing binary

Prebuilt runs are sequential and **fail fast by default**. Add `--no-fail-fast` to run every selected binary even when an earlier binary fails, crashes or cannot be launched:

```sh
cargo test-r run --binaries-metadata binaries.json --no-fail-fast -- --retry-failed 2
cargo test-r run --selection selection.json --no-fail-fast
cargo test-r run --target-dir archive-target --no-fail-fast
```

This applies equally to metadata, saved selections and extracted archives. Binary-id order, package working directories and harness arguments are unchanged. Passing binaries are not rerun, and per-test retries inside each binary are unaffected. Each binary inherits stdout and stderr and writes its ordinary reports; cargo-test-r also reports each failing binary's id and status on stderr, so distinct failures remain distinguishable.

The final exit code gives **cancellation precedence over timeout**, then uses the **first failure in selection order**, not the last or largest code:

| Binary outcome | Exit code |
| --- | --- |
| All binaries passed | 0 |
| Invocation cancelled | 128 + signal number (for example, Ctrl+C: 130) |
| Invocation deadline expired | 124 |
| Nonzero exit | The binary's exit code |
| Killed by a signal (Unix) | 128 + signal number |
| Launch failure after validation (for example, an invalid executable image) | 1 |

With default fail-fast, the remaining binaries are not run. With `--no-fail-fast`, later binaries still run but cannot turn the final result into success. A deadline or cancellation always stops admission of further binaries, regardless of `--no-fail-fast`. Artifact validation still happens for the entire selection before any binary starts; `--no-fail-fast` does not bypass invalid metadata or missing files.

### Freezing prebuilt binaries with snapshot

On **Linux with glibc**, `snapshot` copies every binary in a saved selection into an independent directory that Cargo never writes:

```sh
cargo test-r snapshot --selection selection.json --out /ci/snap/4711
# snapshot /ci/snap/4711 sha256:<digest>
cargo test-r verify --selection /ci/snap/4711/selection.json --expect-digest sha256:<digest>
cargo test-r run --selection /ci/snap/4711/selection.json --expect-digest sha256:<digest>
```

The output directory must not exist, and its parent must exist. Snapshot creation requires the original nextest metadata in `inputs`, unchanged (its recorded SHA-256 is checked), to identify mutable Cargo target directories and build-script `linked-paths`. Relative input metadata paths are resolved from the current directory, as with `list`. After publication, neither `verify` nor `run` needs that metadata or the original executables.

Snapshot creation rejects selections containing any Cargo-message input, including mixed inputs, with an explicit unsupported-input error. Cargo streams do not reliably describe the complete mutable target directory and linked-library roots (for example external build-script link search paths). Guessing these from executable filenames would weaken the shared-library contract. Use nextest binaries metadata to create a snapshot; ordinary `run --selection` fully supports Cargo-message selections without snapshot hashes.

Snapshot creation uses a private temporary sibling directory. Each new regular file under `bin/` has a sanitized, length-limited binary id plus the full SHA-256 of that id as its name, so colliding basenames and sanitized ids remain distinct. Copies are never hard links or symlinks. Source and copy hashes must agree; executable permissions are preserved. Files and directories are synced before an atomic rename publishes the complete snapshot, using no-overwrite semantics where supported (see the trust assumptions below); the parent directory is synced afterwards. An error cleans up the temporary directory instead of publishing a partial selection. Working directories remain the original package directories; snapshots do not copy resources or make tests hermetic.

The selection format remains version 1. Ordinary `list` output is unchanged. Snapshot entries add these optional fields (omitted from ordinary selections):

```json
{
  "sha256": "<64 lowercase hex digits of executable contents>",
  "mode": 493,
  "dependencies": [
    { "path": "/lib/x86_64-linux-gnu/libc.so.6", "sha256": "<64 lowercase hex digits>" }
  ]
}
```

`mode` is the Unix permission/special bits (493 is decimal for octal 0755); on non-Unix hosts verification uses the platform's read-only flag (0 or 1). `dependencies` is empty for a static ELF executable. For dynamic executables, snapshot reads the ELF interpreter, `DT_NEEDED` and `RPATH`/`RUNPATH`, then invokes `<interpreter> --list` on the original and copied executables in the recorded working directory. It rejects missing libraries, paths inside target directories or `linked-paths` (including through symlinks), and changed resolution after relocation, including `$ORIGIN`. Admitted libraries and the interpreter are recorded with their loader paths and content hashes. Library contents are not bundled.

`run --selection` automatically verifies the entire selection before starting any binary when any entry contains snapshot fields. Verification requires complete hashes, modes and dependencies on every entry, checks that executables resolve inside the snapshot's `bin/`, and rejects changed or missing files, permissions and libraries. It never executes the loader. `--expect-digest sha256:…` additionally checks the exact selection file bytes, detecting edits to metadata or recorded hashes. Keep that digest separately from the snapshot. Removing all snapshot fields makes an unpinned selection indistinguishable from an ordinary selection, so pin the digest when manifest tampering must be detected.

Both `verify` and `run --selection` accept `--skip-library-hashes`: recorded libraries must still exist, but their contents may differ. Use this deliberately when a host's system libraries have been patched; it does not skip binary hashes, modes or the manifest digest. Verification itself is portable and compiles on all supported platforms, but snapshots are local execution inputs, not cross-platform archives. macOS, Windows and musl reject **snapshot creation** with a not-supported error until their loader contracts are implemented.

**Trust assumptions:** only the CI user may write the snapshot directory (including during verification/execution). Loader inspection runs the system interpreter from **trusted, own build outputs**, not arbitrary downloaded executables. System libraries outside mutable build output are assumed stable for the snapshot's lifetime. Preserve the loader environment (for example `LD_LIBRARY_PATH`/`LD_PRELOAD`), working directories and host loader configuration between snapshot and execution: verification checks the recorded files without resolving libraries again. Hashes detect accidental changes; without a separately trusted digest they do not authenticate a maliciously rewritten manifest. Verification is not a sandbox and does not prevent a concurrent writer changing files after the check.

Publication normally uses `renameat2(RENAME_NOREPLACE)` to reject even a concurrently created output directory. On filesystems that reject this flag with `EINVAL` (for example some NFS or FUSE mounts), snapshot falls back to checking that the output does not exist and then performing an ordinary atomic rename. This fallback has a weaker race guarantee: another process could create an empty output directory between the check and rename and have it replaced. Ensure no other process publishes to the same output path on those filesystems.

### Invocation deadlines and descendant cleanup

```sh
cargo test-r run --selection selection.json --deadline 30m --kill-grace 10s
```

`--deadline` is an optional **whole invocation** budget measured on a monotonic clock from cargo-test-r startup. It includes selection loading and all binaries, including retries inside each binary; it is never reset between binaries. On the Cargo fallback, the supervised process is `cargo test`, so **compilation is included**. Validation and metadata loading are synchronous; expiry is checked before launching work, not by interrupting filesystem reads.

Durations are nonnegative integers followed by `ms`, `s`, `m`, or `h` (for example, `90s`, `30m`, `1h`). Fractions, bare numbers, and overflowing durations are rejected. `0s` expires before starting a binary. `--kill-grace` defaults to `10s` and is separate from the deadline: shutdown may extend beyond the invocation budget.

On Unix every binary (or the fallback Cargo process) starts in its own process group. SIGINT, SIGTERM, and SIGHUP are recorded and forwarded by the supervisor. On expiry it sends TERM; on cancellation it sends the received signal. After the grace interval it escalates to KILL. A second Ctrl+C skips the remaining grace. Signal handlers only record cancellation and wake the supervision loop, so cancellation around spawning cannot abandon an untracked child. stdout and stderr remain inherited; output and partial reports already written are not removed.

In prebuilt mode, cleanup also runs **after every normal binary exit**, without changing a passing/failing exit into a different test result. This prevents orphan descendants from retaining an inherited pipe or listening port. Use `--keep-leftovers` for a suite that intentionally leaves a daemon alive after a normal exit. Deadline expiry and cancellation still clean up with this flag. The Cargo fallback does not clean leftovers after normal completion.

If cleanup itself fails, no further binaries are admitted; an otherwise successful invocation exits 1. An existing test failure, timeout, or cancellation keeps its exit-code precedence. After KILL the supervisor allows up to two seconds for process draining instead of waiting indefinitely for an unkillable process.

Platform containment and limits:

- **Linux:** process groups plus `PR_SET_CHILD_SUBREAPER`. Orphans are adopted, terminated, and reaped, including descendants that changed session/process group. This requires readable `/proc` child information. It is not isolation against another PID namespace, a service manager such as systemd, Docker, or privileged processes. Cleanup polling after KILL is bounded; uninterruptible kernel waits cannot be made killable by a userspace runner.
- **macOS:** process groups only. Descendants that call `setsid` or `setpgid` can escape; orphan zombies belong to the system reaper, not cargo-test-r. Do not use this as a containment boundary for daemonizing or hostile workloads.
- **Windows:** cargo-test-r joins a Job Object before launching work; descendants inherit membership. Both normal cleanup and deadline/cancellation enumerate and terminate job members **except the runner**, with bounded waits and **without a grace period**. The runner stays alive to report the outcome and return the usual exit code. Console Ctrl+C maps to signal 2 (exit 130), and Ctrl+Break to signal 15 (exit 143). Console close/logoff and forcible termination of the supervisor are not graceful cancellation; use Ctrl+C, Ctrl+Break, or the deadline. Job assignment failure (for example, incompatible enclosing job restrictions) is an error before tests start. Job membership is not a sandbox or a guarantee against privileged external service managers.
