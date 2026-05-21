# Dependency sharing strategies

By default, every `#[test_dep]` is created once per test binary run and shared by all tests in that suite. This is convenient, but it interacts poorly with output capturing: because the materialised value lives inside the parent test runner process, capturing forces a single-threaded fallback when at least one such dep exists.

test-r supports per-dependency **sharing strategies** that let individual deps opt into safer-to-parallelise lifetimes:

| Strategy     | One instance per…                       | Constructor runs in… | Parallel under capture? |
|--------------|-----------------------------------------|----------------------|-------------------------|
| `Shared`     | suite                                   | parent               | No (default, today)     |
| `PerWorker`  | worker process                          | each worker          | Yes                     |
| `Cloneable`  | suite (parent) + per worker copy        | parent               | Yes                     |
| `Hosted`     | suite (parent owns) + per worker handle | parent               | Yes                     |
| `HostedRpc`  | suite (parent owns) + per worker stub   | parent               | Yes                     |

The default remains `Shared`, so any existing `#[test_dep]` keeps working unchanged.

## Choosing a strategy

- Use **`Shared`** when the dep owns process-local state that genuinely cannot be duplicated (single global resource) AND a small descriptor cannot represent it for workers.
- Use **`PerWorker`** when re-running the constructor on each worker is cheap and tests are happy with their own private instance (temp dirs, caches, in-memory stores).
- Use **`Cloneable`** when the constructor is expensive (compilation, parsing a large schema, fetching once over the network) but the resulting value can be cheaply round-tripped through a byte buffer. The parent runs the constructor exactly once and ships the wire form to every worker, where each worker reconstructs a local copy.
- Use **`Hosted`** when the dep owns a long-lived singleton service (TCP listener, Docker container, env-based test environment, gRPC server) that must NOT be duplicated across worker processes, but workers need a small handle (an address, a port, a credentials bundle) to reach it.
- Use **`HostedRpc`** when the dep is a singleton that exposes a small, in-process Rust API (e.g. "give me the next unique id"), and you do not want to set up a real network protocol just to share it with worker subprocesses. The runtime provides the IPC channel; you provide the owner type, a stub, and a method dispatcher.

## `PerWorker`

Annotate the constructor with `scope = PerWorker`:

```rust
use test_r::{test, test_dep};

pub struct WorkerScratchDir(pub tempfile::TempDir);

#[test_dep(scope = PerWorker)]
fn create_scratch_dir() -> WorkerScratchDir {
    WorkerScratchDir(tempfile::tempdir().expect("scratch dir"))
}

#[test]
fn writes_a_file(scratch: &WorkerScratchDir) {
    let path = scratch.0.path().join("hello.txt");
    std::fs::write(&path, b"hi").unwrap();
    assert!(path.exists());
}
```

When the runner spawns worker children for output capturing, each worker materialises `WorkerScratchDir` independently. Tests scheduled on the same worker share the same instance; tests scheduled on different workers see independent instances.

## `Cloneable`

Implement the `CloneableDep` trait for the dependency type, then annotate the constructor with `scope = Cloneable`:

```rust
use test_r::core::CloneableDep;
use test_r::{test, test_dep};

pub struct PrecomputedPayload {
    pub bytes: Vec<u8>,
}

impl CloneableDep for PrecomputedPayload {
    fn to_wire(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    fn from_wire(bytes: &[u8]) -> Self {
        Self { bytes: bytes.to_vec() }
    }
}

#[test_dep(scope = Cloneable)]
fn create_payload() -> PrecomputedPayload {
    // Runs exactly once, in the parent process.
    PrecomputedPayload { bytes: expensive_build() }
}

#[test]
fn uses_payload(payload: &PrecomputedPayload) {
    assert_eq!(payload.bytes.len(), 1024 * 1024);
}
# fn expensive_build() -> Vec<u8> { vec![0; 1024 * 1024] }
```

The wire encoding is entirely up to the implementor — there is no `serde` requirement. Cloneable wire payloads larger than 64 KiB are supported (the IPC framing uses a `u32` length prefix).

### Cloneable restrictions

To keep the runtime change small in this phase, `Cloneable` deps are subject to two restrictions:

1. The constructor must not take other `#[test_dep]` parameters. Build any prerequisite state inside the constructor instead. (The tokio variant supports `async fn` constructors — they are awaited on the parent.)
2. The `CloneableDep::from_wire` reconstruction runs with no other worker-local context. If you need a worker-local prerequisite (e.g. a per-worker engine to deserialise into), use `PerWorker` for that prerequisite and let your higher-level code combine the two.

## `Hosted`

Implement the `HostedDep` trait for the dependency type, then annotate the constructor with `scope = Hosted`:

```rust
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use test_r::core::HostedDep;
use test_r::{test, test_dep};

pub struct LiveService {
    addr: SocketAddr,
    /// Owner-only: the parent holds the live listener for the whole suite.
    /// Workers never populate this.
    _listener: Option<Arc<TcpListener>>,
}

impl LiveService {
    fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        // ... spawn an accept loop, etc.
        Self { addr, _listener: Some(Arc::new(listener)) }
    }
}

impl HostedDep for LiveService {
    fn descriptor(&self) -> Vec<u8> {
        // Ship just enough information for workers to reach the live owner.
        self.addr.to_string().into_bytes()
    }

    fn from_descriptor(bytes: &[u8]) -> Self {
        let s = std::str::from_utf8(bytes).expect("utf-8");
        let addr: SocketAddr = s.parse().expect("addr");
        Self { addr, _listener: None /* workers don't own the listener */ }
    }
}

#[test_dep(scope = Hosted)]
fn live_service() -> LiveService {
    // Runs exactly once, in the parent process. Stays alive for the
    // entire suite — workers reach it via the descriptor.
    LiveService::bind()
}

#[test]
fn worker_can_reach_owner(service: &LiveService) {
    // service.addr points at the listener held by the parent.
    // ...
}
```

### How Hosted works

1. The parent test runner calls the owner constructor **once** when it first builds the execution plan.
2. The parent calls `HostedDep::descriptor()` on the owner and ships the bytes to every worker via IPC (`ProvideHostedDescriptor`).
3. Each worker calls `HostedDep::from_descriptor(bytes)` to materialise a local handle that knows how to reach the parent-held owner.
4. The parent keeps the owner alive (in `_hosted_owners`) for the entire suite, so workers can rely on it being reachable as long as any test is still running.
5. When the suite finishes, the parent drops the owner — typically triggering whatever cleanup the owner's `Drop` implementation needs (closing the listener, stopping a container, etc.).

The wire encoding is entirely up to the implementor, exactly like `Cloneable`. Descriptors are usually small — an address, a port, a credentials bundle.

### Hosted restrictions

- The constructor must not take other `#[test_dep]` parameters. Owner-side dependency wiring is reserved for a future phase.
- The owner type and the worker handle type share the same Rust type (`Self`). The implementor is responsible for keeping owner-only fields (sockets, accept loops, container handles) in `Option`s or `Arc`s that workers don't populate.

### Mode-consistent semantics across `--nocapture` and worker mode

The test functions always see the **worker-side handle** produced by
`HostedDep::from_descriptor`, never the raw owner value, regardless of which
execution mode the runner ended up in:

- With output capturing on (the default), every test runs inside an IPC
  worker subprocess and sees `from_descriptor(parent_descriptor_bytes)`.
- With `--nocapture` (or single-process mode), the runner still creates the
  owner exactly once in the parent, calls `descriptor()` on it, and then
  locally applies `from_descriptor` so the in-process tests see exactly the
  same kind of handle.

This means you can write `HostedDep::from_descriptor` as the single source
of truth for what a test-visible handle looks like, and you don't need to
distinguish between "parent test run" and "worker test run" in your test
code.

## `HostedRpc`

`HostedRpc` is the close sibling of `Hosted` for singletons whose
test-visible API is a small set of method calls rather than a network
endpoint. The owner lives in the top-level parent for the entire suite
and workers see a **stub** — a tiny Rust struct that serialises each
method call, ships it over the runtime's IPC channel to the parent, and
unwraps the reply.

Use this when:

- the dep is a singleton (an id allocator, a leadership coordinator, a
  globally-shared counter, …),
- you don't want to invent and embed a real network protocol for tests,
- a few hundred call-per-test of overhead per RPC are acceptable
  (every call is one synchronous round-trip on the existing IPC socket).

### Phase 1C MVP scope

- Sync runner only. The tokio runner explicitly rejects `HostedRpc`
  deps with a clear panic message; full async support is deferred to a
  later phase.
- The user implements the owner type, the worker-visible stub type, and
  one method-dispatch function on the owner. The macro wires those into
  the runtime; there is no `#[hosted_rpc]` codegen yet.
- One in-flight call at a time per worker subprocess. Each `stub.foo()`
  takes the IPC connection lock, writes the request frame, reads exactly
  one reply frame, and returns. No multiplexer or out-of-order replies.

### `HostedRpcDep` (the trait)

```rust
use test_r::core::{HostedRpcChannel, HostedRpcDep, HostedRpcError};

pub struct LastUniqueIdOwner { counter: std::sync::Mutex<u64> }

const METHOD_NEXT: u32 = 1;

impl HostedRpcDep for LastUniqueIdOwner {
    /// The worker-visible handle tests parameterise on.
    type Stub = LastUniqueIdStub;

    /// Owner-side dispatcher. `method_idx` is a stable, user-chosen
    /// index per method (you pick the numbering). `args` is the raw
    /// serialised payload; the choice of codec is yours.
    fn dispatch(&mut self, method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
        match method_idx {
            METHOD_NEXT => {
                let mut guard = self.counter.lock().map_err(|e| e.to_string())?;
                *guard += 1;
                Ok(guard.to_be_bytes().to_vec())
            }
            other => Err(format!("unknown method_idx {other}")),
        }
    }

    /// Worker-side stub builder. The runtime hands you a fresh
    /// `HostedRpcChannel` tagged with this dep's fully-qualified id
    /// once per worker; you wrap it in your stub.
    fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
        LastUniqueIdStub { channel }
    }
}

pub struct LastUniqueIdStub { channel: HostedRpcChannel }

impl LastUniqueIdStub {
    pub fn next(&self) -> Result<u64, HostedRpcError> {
        let bytes = self.channel.call(METHOD_NEXT, Vec::new())?;
        let arr: [u8; 8] = bytes.as_slice().try_into()
            .map_err(|e| HostedRpcError::Transport(format!("{e}")))?;
        Ok(u64::from_be_bytes(arr))
    }
}

#[test_dep(scope = HostedRpc, stub = LastUniqueIdStub)]
fn unique_id_owner() -> LastUniqueIdOwner {
    LastUniqueIdOwner { counter: std::sync::Mutex::new(0) }
}

#[test]
fn ids_are_unique(ids: &LastUniqueIdStub) {
    let a = ids.next().unwrap();
    let b = ids.next().unwrap();
    assert!(a < b);
}
```

The `stub = StubType` attribute is required. The constructor returns the
**owner type**, the test parameter is the **stub type**; the macro
registers the dep under the stub's type name so test parameter resolution
finds it.

### How HostedRpc works

1. The parent test runner calls the owner constructor **once** when it
   first builds the execution plan, wraps the result in an internal
   `HostedRpcOwnerCell`, and keeps it alive for the whole suite.
2. Each worker subprocess builds a stub via `build_stub(channel)` using
   an IPC-backed `HostedRpcChannel` keyed to the dep's fully-qualified id.
3. When a test calls `stub.foo(args)`, the stub sends an `IpcResponse::HostedRpcCall`
   frame on the shared IPC socket and blocks for the matching
   `IpcCommand::HostedRpcReply`.
4. The parent's worker dispatch loop intercepts incoming `HostedRpcCall`
   frames, looks up the right `HostedRpcOwnerCell` by dep id, runs the
   owner's `dispatch(method_idx, &args)` behind a `Mutex`, and writes
   the reply back.
5. Owner panics are caught by `HostedRpcOwnerCell::dispatch` and surfaced
   to the calling worker as `HostedRpcError::Dispatch("hosted rpc owner panicked: …")`.
   The mutex is poisoned after a panic; subsequent calls then return a
   stable `"hosted rpc owner poisoned"` error so a single bad call
   doesn't bring down the rest of the suite.
6. In `--nocapture` / single-process mode, the runtime swaps the
   IPC-backed transport for `InProcessHostedRpcTransport`, which calls
   the owner cell directly — tests see the same stub regardless of
   execution mode.

### HostedRpc restrictions

- The constructor must be **synchronous** in the Phase 1C MVP.
- The constructor must not take other `#[test_dep]` parameters (mirrors
  `Hosted`).
- The constructor must return the **owner type**; tests must parameterise
  on the **stub type** named via `stub = StubType`.
- One in-flight RPC at a time per worker subprocess. Pipelined or
  concurrent calls are not supported in the MVP.
- The tokio runner does not currently support `HostedRpc` deps.

### MVP temporal invariant — when stub calls are safe

The Phase 1C transport intentionally reuses the same IPC socket the
harness uses for `RunTest` / `ProvideCloneable` /
`ProvideHostedDescriptor` traffic. Stubs share that socket with the
worker's main IPC command loop, and they take a process-local mutex
around one full request/response. The IPC framing only stays in sync
because of two assumptions:

1. **Stubs are only invoked from inside the test body.** Tests in the
   worker subprocess only run between `RunTest` (parent → worker) and
   `TestFinished` (worker → parent). During that window the worker's
   main command loop is idle (it doesn't read from the socket), so the
   stub's request/reply round-trip is the only traffic in flight.

2. **`HostedRpcDep::build_stub` is cheap and side-effect free.** It runs
   once per worker subprocess at startup, *before* the worker has
   received its first `RunTest`. If `build_stub` itself called
   `channel.call(...)`, the parent could legally send a `RunTest` while
   the stub was blocked waiting for a reply, and the transport would
   read that `RunTest` as if it were a `HostedRpcReply`.

Concretely:

- ✅ Calling `stub.foo(...)` from inside a `#[test]` function body, or
  from any helper the test body awaits or blocks on, is safe.
- ❌ Calling `channel.call(...)` from `HostedRpcDep::build_stub` is not
  supported and will desync the IPC framing.
- ❌ Calling `stub.foo(...)` from a detached background thread that
  outlives the test body is not supported — once the test returns, the
  worker's next IPC traffic is `TestFinished` followed by either
  `RunTest` or `Provide*` from the parent, not a `HostedRpcReply`.
- ❌ Calling `stub.foo(...)` from `Drop` / destructor-style cleanup, or
  from any teardown hook that may fire after the test body has
  returned, is not supported for the same reason — the rule is
  "inside the test body, every time".

If you need any of the unsupported shapes, treat that as a signal to
either restructure your test (run the work inside the test body, wait
for the background thread to finish before returning) or to wait for a
post-MVP Phase HR1.x with a dedicated worker-side reader and a waiter
table.

### When to prefer `Hosted` over `HostedRpc`

If your dep already exposes a network endpoint (TCP listener, gRPC
server, Docker container with a published port), use `Hosted` and ship
the address as the descriptor. `HostedRpc` is the right choice when no
such endpoint exists and you don't want to invent one.

## When does the single-thread fallback still kick in?

The parallel/single-thread decision is made once, after the dep graph is known:

- If output capturing is **off** (`--nocapture`), the runner never falls back.
- If capturing is **on** and the suite has at least one **`Shared`** dep, the runner falls back to one thread.
- If capturing is **on** and all deps in scope are `PerWorker`, `Cloneable`, `Hosted`, and/or `HostedRpc`, the runner stays parallel.

A suite that mixes `Shared` and any of the parallel-safe scopes will still fall back: `Shared` is the strictest scope in scope-mixing today. Migrate the remaining `Shared` deps to a more permissive scope to recover parallelism.
