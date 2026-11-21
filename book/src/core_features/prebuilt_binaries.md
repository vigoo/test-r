# Running prebuilt test binaries

`cargo test-r run` normally forwards to `cargo test`. It can also run test binaries that were built earlier, for example in a separate CI step, without invoking Cargo. This chapter covers:

- [choosing the binaries](#choosing-binaries) and saving the choice as a **selection** file,
- [snapshots](#snapshots-freezing-binaries) that freeze binaries so a later rebuild cannot change them,
- [deadlines, cancellation and cleanup](#deadlines-cancellation-and-cleanup) of the processes tests leave behind,
- an [event log](#event-log) that records what happened, for CI tooling.

```sh
# Build once
cargo nextest list --list-type binaries-only --message-format json > binaries.json
# Choose binaries and save the choice
cargo test-r list --binaries-metadata binaries.json -p my-crate > selection.json
# Run exactly those binaries
cargo test-r run --selection selection.json --no-fail-fast --deadline 30m -- --retry-failed 2
```

## Choosing binaries

### Inputs

cargo-test-r reads one of three kinds of build output. None of them makes cargo-test-r invoke Cargo.

| Input | How to produce it | Flag |
| --- | --- | --- |
| nextest binary list | `cargo nextest list --list-type binaries-only --message-format json > binaries.json` | `--binaries-metadata binaries.json` |
| Cargo JSON messages | `cargo test --no-run --message-format json > cargo-messages.jsonl` | `--cargo-messages cargo-messages.jsonl` |
| Extracted nextest archive | `cargo test-r reuse-nextest-archive --archive-file tests.tar.zst` | none: used automatically when no other input is given |

Both flags can be repeated, and both kinds can be mixed. An extracted archive lives in `target/nextest-archive`, or in `<DIR>/nextest-archive` with `--target-dir <DIR>`. Its binary paths and package directories are moved to match the current workspace.

For Cargo messages, use Cargo's JSON stdout, not its human-readable stderr. Only test artifacts that have an executable are used; other JSON messages are skipped. Every line must still be valid JSON.

Without any input, `cargo test-r run` forwards all its arguments to `cargo test` (see [Without prebuilt input](#without-prebuilt-input)).

### Filters

`cargo test-r list` and prebuilt `cargo test-r run` accept the same filters:

- `-p`/`--package <NAME>` and `--exclude <NAME>` match **Cargo package names**, not directory names. Both accept globs and can be repeated. Excludes apply even without `-p`.
- `--lib` selects library unit tests (including proc macros). `--bins` selects binary unit tests. `--test <NAME>` selects integration tests by name, with globs, and can be repeated. `--tests` selects all three kinds.
- Kind filters add up: `--lib --test integration` selects both. With no kind filter, every binary is eligible.
- `--binary-id <ID>` keeps only the given binary ids, and can be repeated.

Binary ids follow nextest's scheme: `my-crate` for library unit tests, `my-crate::integration` for an integration test, and `my-crate::bin/tool` for other targets. They come from the package id and target, never from file names.

The same binary id listed twice with identical details is merged. Two different entries for one id, such as two executable paths or two build variants, are an error that shows both, rather than one being picked silently. A binary id is unique within a selection.

`--expect-count <N>` requires exactly N binaries after filtering and deduplication. An empty selection is always an error. For scripts that need one executable path rather than a selection file, `list --output executable-path` requires exactly one binary and prints its absolute path followed by a newline. It rejects paths containing line breaks; JSON output supports those paths.

Test name filters and arguments after `--` are passed to every selected binary:

```sh
cargo test-r run --binaries-metadata binaries.json -p my-crate --test integration -- --retry-failed 2
```

Build options such as `--release`, `--features` or `--no-run` are rejected, because the binaries are already built.

### Checking Cargo artifact identities

When consuming Cargo messages, `--cargo-metadata <FILE>` checks each selected artifact against an existing `cargo metadata --no-deps --format-version 1` document. It does not invoke Cargo:

```sh
cargo metadata --no-deps --format-version 1 > cargo-metadata.json
cargo test --locked -p my-crate --lib --no-run --message-format json > cargo-messages.jsonl
cargo test-r list --cargo-messages cargo-messages.jsonl --cargo-metadata cargo-metadata.json \
  -p my-crate --lib --expect-count 1 --output executable-path
```

This checks the package id and name, workspace membership, package source directory and manifest, and the target's name, original kind list and source file. Paths are canonicalized. Each selected raw artifact is checked before duplicate entries are merged, so a duplicate with a different source file cannot be hidden by a valid entry. Unselected artifacts do not need to belong to the supplied workspace. Local workspace members may live outside the workspace-root directory, as Cargo permits. Both the build messages and metadata are trusted inputs; produce them for the intended workspace and build settings.

`--cargo-metadata` requires Cargo-message inputs only. It cannot be combined with nextest inputs, an extracted archive or a saved selection. Its path and digest are recorded as an input with kind `cargo-metadata`. Without this option, Cargo-message selection retains its existing behavior.

### Selection files

`cargo test-r list` prints the chosen binaries as JSON on stdout without invoking Cargo or test binaries. `cargo test-r run --selection <FILE>` later runs those binaries, optionally narrowed by filters, in `binary-id` order without reading the inputs again:

```json
{
  "format": "cargo-test-r-selection",
  "version": 1,
  "inputs": [
    { "path": "binaries.json", "sha256": "<64 hex digits>", "kind": "nextest-binaries" }
  ],
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

- `inputs` records which files the selection came from, with the SHA-256 of their contents. `kind` is `nextest-binaries`, `cargo-messages` or `cargo-metadata`; a missing `kind` means `nextest-binaries`. `run` ignores input kinds it doesn't know, so selections from newer versions still run.
- `build-variant` is an opaque short hash of the build settings. Compare it only between selections made by the same cargo-test-r version from the same kind of input.
- Paths are absolute. Each binary runs in its package's directory.

Package, target and binary-id filters can narrow a saved selection without reading its inputs or rewriting its manifest. The complete saved selection is validated, and a snapshot is verified in full, **before** filtering. A missing or tampered unselected executable therefore still fails the run. `--expect-digest` always identifies the original file's exact bytes, never a derived subset. `--expect-count` applies to the filtered set. `--target-dir` and other metadata inputs cannot be combined with `--selection`.

```sh
cargo test-r run --selection /ci/snap/run-1/selection.json --expect-digest sha256:<digest> \
  -p my-crate --test integration --expect-count 1 -- --retry-failed 2
```

A selection does not freeze the executables: a rebuild can replace them. Use a [snapshot](#snapshots-freezing-binaries) for that.

### Errors before anything runs

Every selected binary is checked before the first one starts. Unreadable input, an empty selection, a missing executable or working directory, and a file that is not executable (Unix permission bits; an `.exe` extension on Windows) all fail with exit code 1. `list` then prints nothing on stdout.

## Running the binaries

Binaries run one at a time, in `binary-id` order, with stdout and stderr inherited.

### Failing fast, or not

By default the run stops after the first binary that fails, crashes or cannot be started. `--no-fail-fast` runs every binary anyway. Passing binaries are never rerun; per-test retries such as `--retry-failed` still happen inside each binary. Each failing binary's id and status are printed on stderr.

### Exit code

| What happened | Exit code |
| --- | --- |
| Everything passed | 0 |
| Cancelled by a signal | 128 + signal (Ctrl+C: 130) |
| Deadline expired | 124 |
| A binary failed | that binary's exit code (the **first** failure in selection order) |
| A binary was killed by a signal (Unix) | 128 + signal |
| A binary could not be started | 1 |
| Everything passed, but cleanup or the event log failed | 1 |

Cancellation wins over the deadline, and the deadline wins over binary failures. A binary that itself exits with 124 or 128 + n looks like a timeout or a signal in the exit code; the [event log](#event-log) tells them apart.

### Without prebuilt input

Without `--selection`, `--binaries-metadata`, `--cargo-messages` or an extracted archive, `cargo test-r run` forwards its arguments to `cargo test`. Without `--deadline`, cargo-test-r prints the command and runs `cargo test` as a direct call would, with the same signals and terminal. With `--deadline`, the [deadline and cancellation rules](#deadlines-cancellation-and-cleanup) apply to the whole `cargo test` process, compilation included; processes left behind after `cargo test` finishes normally are not stopped (on Windows, see the platform notes). `--events` and `--binary-id` need prebuilt input.

## Snapshots: freezing binaries

A selection points at executables inside Cargo's target directory, which a later build can overwrite. On **Linux with glibc**, a snapshot copies them into a directory that Cargo never writes to:

```sh
cargo test-r snapshot --selection selection.json --out /ci/snap/run-1
# prints: snapshot /ci/snap/run-1 sha256:<digest>
cargo test-r verify --selection /ci/snap/run-1/selection.json --expect-digest sha256:<digest>
cargo test-r run --selection /ci/snap/run-1/selection.json --expect-digest sha256:<digest>
```

The output directory must not exist yet; its parent must. The snapshot appears all at once or not at all. It contains `bin/` with one copy of each executable, and a `selection.json` that points at those copies. Working directories stay the original package directories: test resources are not copied.

### What a snapshot records

The snapshot's `selection.json` is a normal version 1 selection with extra fields:

```json
{
  "format": "cargo-test-r-selection",
  "version": 1,
  "inputs": [ ... ],
  "binaries": [{
    "binary-id": "my-crate::integration",
    ...,
    "sha256": "<64 hex digits of the executable>",
    "mode": 493,
    "dependencies": [
      { "path": "/lib/x86_64-linux-gnu/libc.so.6", "sha256": "<64 hex digits>" }
    ]
  }],
  "snapshot": { "platform": "linux-gnu" }
}
```

- `snapshot` marks the file as a snapshot, and names the rules used to create it.
- `mode` is the executable's Unix permission bits (493 is 0755).
- `dependencies` lists the shared libraries the system loader resolved for the executable, plus the loader itself, with their hashes. It is empty for a static executable.

`snapshot` refuses to create a snapshot when a library is missing, when a library comes from the target directory or a build script's link path (so a rebuild could change it), or when copying the executable changes how its libraries resolve (for example through `$ORIGIN`). It needs the original nextest binary list from `inputs`, unchanged, to know which directories are build output. Selections made from Cargo messages cannot be snapshotted, because Cargo's messages don't describe those directories completely.

### Verification

`verify`, and `run --selection` on a snapshot, check everything before any binary starts: each executable must be inside the snapshot's `bin/` and match its hash and mode, and each recorded library must exist and match its hash. Verification never runs the loader or a test.

- `--expect-digest sha256:<digest>` also checks the exact bytes of `selection.json`. Keep the digest separately from the snapshot: it is what detects an edited manifest.
- `--skip-library-hashes` still requires recorded libraries to exist, but allows their contents to change, for example after a system update. Binary hashes and the digest are still checked.
- A file with snapshot fields but no `snapshot` marker, an unknown key inside `snapshot`, or a platform this version doesn't know is rejected rather than run unverified.

Snapshot creation works on Linux with glibc only; macOS, Windows and musl report it as unsupported. Verify and run a snapshot on the same kind of system that created it: paths, file modes and libraries are platform-specific.

**Trust assumptions.** Only the CI user should be able to write to the snapshot directory. Snapshot creation runs the system loader on the executables, so use it on your own build output. System libraries outside the build output are assumed stable while the snapshot is used. Keep the loader environment (such as `LD_LIBRARY_PATH`) and working directories the same between snapshot and run: verification checks the recorded files, it does not resolve libraries again. Without a separately kept digest, hashes detect accidents, not deliberate tampering. Verification is not a sandbox: a process changing files after the check is not detected.

On filesystems that cannot rename without replacing (some NFS or FUSE mounts), or in containers that block that call, publishing falls back to "check, then rename". Make sure no other process publishes to the same output path at the same time there.

## Deadlines, cancellation and cleanup

```sh
cargo test-r run --selection selection.json --deadline 30m --kill-grace 10s
```

- `--deadline` is a budget for the **whole invocation**, measured from when cargo-test-r starts. It covers loading and every binary, and is never reset between binaries. On the Cargo fallback it includes compilation. When it expires, no further binaries start and the running one is stopped. Durations are whole numbers with `ms`, `s`, `m` or `h`, such as `90s` or `30m`; `0s` stops before the first binary.
- **Cancellation**: Ctrl+C, SIGTERM or SIGHUP while binaries run stops the run the same way. A second signal (Ctrl+C, SIGTERM or SIGHUP) skips the grace period. During loading and verification, before anything has started, these signals simply end cargo-test-r.
- **Stopping**: on Unix the running binary and its descendants get SIGTERM, or the received signal when cancelled; after `--kill-grace` (default `10s`) they get SIGKILL. The grace period can extend past the deadline. Output and reports already written are kept.
- **Leftover processes**: in prebuilt runs, processes a binary leaves behind (such as a server that keeps a port open) are stopped after **every** binary, even when it passed. `--keep-leftovers` turns this off for suites that start a daemon on purpose; a deadline or cancellation still cleans up. Cleanup after a binary can itself cross the deadline: the run then exits 124 even though every binary passed.
- If cleanup fails, no further binaries start, and an otherwise passing run exits 1.

### Preparation deadlines

`list`, `snapshot` and `verify` also accept `--deadline` and `--kill-grace`. Either option enables a contained worker supervised by the same process owner used for test binaries. Without either option, preparation runs directly in the caller's process group. The deadline can interrupt blocking input reads, hashing and ELF loader inspection, rather than waiting for them to return. Supervised preparation cleans descendants after success and failure as well as cancellation and deadline expiry. Exit codes use the same signal/deadline precedence as supervised runs. The default kill grace is `10s` when a deadline is supplied alone.

```sh
cargo test-r list --binaries-metadata binaries.json --deadline 10m > selection.json
cargo test-r snapshot --selection selection.json --out /ci/snap/run-1 --deadline 10m --kill-grace 10s
cargo test-r verify --selection /ci/snap/run-1/selection.json --deadline 10m
```

The preparation owner holds the snapshot's staging directory until the worker and descendants have stopped. Cancellation before publication removes partial files; successful publication preserves the snapshot. If process cleanup fails, the staging directory is retained and its path is reported. The owner's initial staging-directory creation and final filesystem cleanup remain synchronous; deadlines bound worker operations, not those owner operations. Grace and process drain can extend past the budget. SIGKILL of the Unix owner prevents it from cleaning up. Keep an outer CI timeout with headroom, and keep preparation and workload-group budgets separate.

These options do not change `run`'s loading behavior: its budget includes selection loading and snapshot verification, but expiry is acted on once that synchronous preparation returns. Use a supervised `verify` command or an outer task deadline when that phase also needs interruption.

### Platform notes

- **Linux:** each binary runs in its own process group, and cargo-test-r adopts orphaned descendants, so processes that start a new session are still found and stopped. This needs `/proc` with child lists (`CONFIG_PROC_CHILDREN`); without them every supervised run fails before starting. It does not reach processes in other PID namespaces or ones started through a service manager such as systemd or Docker.
- **macOS:** process groups only. A descendant that starts a new session or process group escapes cleanup. Don't rely on it to contain daemonizing or hostile programs.
- **Windows:** cargo-test-r and all descendants share a Job Object. Cleanup terminates every member except cargo-test-r, **without** a grace period (`--kill-grace` has no effect). Ctrl+C counts as signal 2 (exit 130) and Ctrl+Break as signal 15 (exit 143). When cargo-test-r exits, or is killed, any processes still in the job are terminated: leftovers kept with `--keep-leftovers`, and processes left by `cargo test` on the fallback. The job allows breakaway: a process created with the `CREATE_BREAKAWAY_FROM_JOB` flag by its parent is not a member, so it survives the run, but it also escapes cleanup on a deadline or cancellation. Failing to create the job is an error before tests start.
- **Terminal (Unix):** because binaries run in their own process group, a test that reads from the terminal is stopped by the system (SIGTTIN), and Ctrl+Z or Ctrl+\ reach cargo-test-r, not the test. Give such tests input another way. The Cargo fallback without `--deadline` is not affected.

## Event log

`--events <FILE>` writes one JSON object per line describing the run. It works for prebuilt runs only.

```sh
cargo test-r run --selection selection.json --invocation-id ci-run-1-shard-3 \
  --events target/events.jsonl -- --format ctrf --logfile target/ctrf.json
```

The file must not exist yet; it is never overwritten. If it cannot be created, nothing runs and the exit code is 1. `--invocation-id` is any string you choose; without it a UUID is generated.

### Records

Every record has these fields:

| Field | Meaning |
| --- | --- |
| `v` | Schema version, currently `1` |
| `seq` | Record number, starting at 0 and always increasing |
| `time` | UTC time, RFC 3339 with milliseconds |
| `elapsed-ms` | Milliseconds since cargo-test-r started, on the same clock as `--deadline` |
| `invocation-id` | The `--invocation-id`, or the generated UUID |
| `event` | One of the event names below |

| Event | Other fields |
| --- | --- |
| `invocation-started` | `pid`, `selection-sha256`, `binary-ids`, `no-fail-fast`, `deadline-ms` (null without a deadline), `kill-grace-ms`, `keep-leftovers` |
| `binary-started` | `binary-id`, `package-id`, `attempt`, child `pid`, `cwd`, `args` |
| `binary-launch-failed` | `binary-id`, `package-id`, `attempt`, `error` (instead of a start and finish) |
| `binary-finished` | `binary-id`, `attempt`, `outcome`, `exit-code`, `signal`, `duration-ms`, `leftovers-killed`, `reports` |
| `deadline-expired` | `binary-id` (null between binaries), `signal` (null), `at-elapsed-ms` |
| `cancel-requested` | `binary-id` (null between binaries), `signal`, `at-elapsed-ms` |
| `invocation-finished` | `outcome`, `exit-code`, and counts of binaries: `passed`, `failed`, `crashed`, `launch-failed`, `timed-out`, `cancelled`, `not-run` |

- `selection-sha256` identifies the original selection file's bytes, or the resolved selection as compact JSON for other inputs. `binary-ids` lists the admitted binaries in execution order after filtering. Completion counts cover that subset; binaries excluded by filters are not counted as `not-run`.
- Binary `outcome` is `passed`, `failed`, `crashed`, `timed-out` or `cancelled`. `exit-code` is set for `passed` and `failed`, `signal` for `crashed`; otherwise they are null. The invocation `outcome` is `passed`, `failed`, `timed-out` or `cancelled`, and matches the [exit code](#exit-code).
- `attempt` is always 1: cargo-test-r does not rerun binaries. `leftovers-killed` is currently always null.
- Each of `deadline-expired` and `cancel-requested` is written at most once, in the order they happened. Both appear when a signal arrives after the deadline expired; cancellation then decides the outcome. `at-elapsed-ms` is when the stop happened (for the deadline, the deadline itself). The record can be written later, for example after the grace period, so its `elapsed-ms` can be larger.
- `invocation-finished` is written only after everything has stopped. **A log without it is incomplete**: cargo-test-r was killed or crashed. Never read a missing final record as success. Ignore a last line that has no newline.
- If writing the log fails, tests keep running, a warning is printed, and an otherwise passing run exits 1.

### Reports and identity

Each binary gets these environment variables, with or without `--events`:

- `TEST_R_INVOCATION_ID`: the invocation id.
- `TEST_R_BINARY_ID`: the binary id.
- `TEST_R_ATTEMPT`: `1`.

With `--events`, each binary also gets `TEST_R_REPORT_INDEX`, the path of a private file. test-r adds one JSON object per report file it opens, such as `{"path": "/w/target/ctrf-1b9d….json", "format": "ctrf"}`. `binary-finished.reports` lists these objects:

- `[]`: the binary uses a test-r version that supports the index, and opened no report.
- `null`: unknown, for example because the binary uses an older test-r.
- Lines cargo-test-r doesn't understand are skipped.

test-r removes the variable at startup, so nested test-r processes don't write into the outer index. The listed paths are the actual files, after test-r adds its unique suffix. Concurrent runs use separate indexes and logs. Reports are never deleted.

## Supervising a command

`exec` supervises one command without selecting tests, invoking Cargo, parsing output or scheduling work. It also works outside a Cargo workspace:

```sh
cargo-test-r exec --deadline 60m --kill-grace 10s -- cargo make integration-tests
```

Arguments after `--` are passed literally. The command inherits stdin, stdout, stderr, the environment and the working directory. A caller that needs to remove credentials or delay launch until progress is published must provide that boundary itself.

The deadline covers startup and command execution. A zero deadline launches nothing. After the direct command exits, the supervisor drains descendants before returning, but a deadline crossed only during that cleanup does not replace the command's exit status. Cancellation during cleanup still takes precedence. This differs from `run`, whose deadline covers the whole test invocation. An outer command deadline and an inner test invocation deadline can be used together.

Exit codes are the command's code, 127 for a launch failure, 124 for a deadline, or 128 plus the signal number for cancellation or command death on Unix. Cleanup failure changes success to 1 but preserves an existing failure. Deadline expiry and launch failure also print diagnostics to stderr. Standard streams remain open until cleanup completes.

The [platform cleanup limits](#deadlines-cancellation-and-cleanup) also apply to `exec`: Linux adopts orphaned descendants, including those that changed sessions; macOS only contains process groups; Windows uses a Job Object. On Unix the command has its own process group, so terminal input has the same restrictions as prebuilt runs. Killing the Linux supervisor itself with SIGKILL prevents its cleanup. The command supervisor does not control processes created by an external service manager or container daemon.

## Inspecting event evidence

`inspect-events` reads a saved event stream without running tests or opening any indexed report:

```sh
cargo-test-r inspect-events --events target/events.jsonl \
  --invocation-id ci-run-1-shard-3 --report-format ctrf
```

The JSON result has `v` (currently 1), `invocation_id` (null until a valid start), `complete`, `outcome` (null until a valid finish), `reports` (objects with `path` and nullable `format`) and `issues` (strings). A report without a string format retains its path with `format: null`. `--invocation-id` requires an exact identity match. By default, `complete` describes stream validity, not report coverage or workload success. `--report-format` also requires report coverage for every selected binary; it does not filter the returned report identities.

Inspection validates v1 envelope ordering, contiguous sequence numbers, timestamps, monotonic elapsed time, admitted binary identities, binary lifecycle transitions and completion counters. The start must declare `binary-ids`, and the finish must contain all binary counts. Records are admitted atomically: a malformed record contributes neither reports nor state. Only newline-terminated records count. Valid prefix reports survive a later malformed or truncated record, missing finish, or read failure.

Binary execution must follow the declared order, without overlap. A recorded deadline or cancellation permanently stops admission, but the active binary may still finish and contribute reports. A stop naming the next admitted binary before its start records a pre-spawn timeout or cancellation; that slot is not counted as skipped and has no reports. Later invocation-level cancellation can change the final verdict without changing that slot's recorded outcome. Final status uses the runner's verdict: cancellation wins over the deadline, which wins over the first binary failure in selection order. Version 1 does not identify cleanup or event-output failures separately, so an otherwise passing invocation may also finish with `failed`, exit 1. Such a runner failure cannot replace a binary failure code or a stop reason.

Unreadable and incomplete streams produce JSON with `complete: false` and explicit issues; the inspection command still exits zero when it can produce that result. Command-line and output errors remain CLI failures. With `--report-format`, launch failure, skipped binaries, unknown report identity (`reports=null`), no opened report (`reports=[]`), and reports without the requested format remain distinct coverage issues. Without that option, these states do not make a valid, finished stream incomplete. `outcome` describes the valid finish record, not an inferred workload verdict; it can be `passed` while requested report evidence is incomplete.

Report paths are opaque identities, not authorization to open files. The consumer must independently confine artifact paths and validate report existence and contents. Harness retries retain their actual report identities and do not change binary `attempt`, which remains 1. Inspection never changes the workload's exit status.
