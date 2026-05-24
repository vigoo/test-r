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

`Hosted` ships with a **worker-view picker** that selects what shape the
worker side sees: `worker = descriptor` (the default — a reconstructed
handle), `worker = rpc(Trait)` (a method-routing stub), or
`worker = both(Trait)` (both views on a single owner). The latter two are
the HR3.2.0 sugar for what would otherwise be a separate `HostedRpc`
registration or a manually-coordinated pair of registrations; see the
[Hosted](#hosted) and [HostedRpc](#hostedrpc) sections for details.

## Choosing a strategy

- Use **`Shared`** when the dep owns process-local state that genuinely cannot be duplicated (single global resource) AND a small descriptor cannot represent it for workers.
- Use **`PerWorker`** when re-running the constructor on each worker is cheap and tests are happy with their own private instance (temp dirs, caches, in-memory stores).
- Use **`Cloneable`** when the constructor is expensive (compilation, parsing a large schema, fetching once over the network) but the resulting value can be cheaply round-tripped through a byte buffer. The parent runs the constructor exactly once and ships the wire form to every worker, where each worker reconstructs a local copy.
- Use **`Hosted`** when the dep owns a long-lived singleton service (TCP listener, Docker container, env-based test environment, gRPC server) that must NOT be duplicated across worker processes, but workers need a small handle (an address, a port, a credentials bundle) to reach it.
- Use **`HostedRpc`** (or equivalently, **`Hosted` with `worker = rpc(Trait)`**) when the dep is a singleton that exposes a small, in-process Rust API (e.g. "give me the next unique id"), and you do not want to set up a real network protocol just to share it with worker subprocesses. The runtime provides the IPC channel; you provide the owner type, a trait (or hand-written stub), and a method dispatcher.
- Use **`Hosted` with `worker = both(Trait)`** when the same owner needs to serve **both** a bulk-data descriptor handle (typically a connection address used by a gRPC client) and a small RPC control surface (kill / flush / snapshot). One owner, two worker-side views, no duplication.

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

### Observing the worker index

`PerWorker` constructors (and the tests they feed) can read the zero-based index the parent assigned to the current worker via `test_r::worker_index()`. Use it to partition a global namespace so that workers cannot collide without coordination.

```rust
use std::sync::atomic::AtomicU16;
use test_r::test_dep;

pub struct LastUniqueId {
    pub id: AtomicU16,
}

#[test_dep(scope = PerWorker)]
fn last_unique_id() -> LastUniqueId {
    // Reserve the high 8 bits for the worker index, leaving 8 bits per
    // worker for the local sequence.
    let seed = (test_r::worker_index() as u16 & 0xFF) << 8;
    LastUniqueId { id: AtomicU16::new(seed) }
}
```

When the runner does not spawn workers — e.g. under `--nocapture`, when no test in the schedule requires capture, or when a `Shared` dep forces the single-thread fallback — `worker_index()` returns `0`. This is the same value the top-level parent observes for itself.

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

`scope = Hosted` keeps the owner alive in the parent for the whole
suite and lets each worker subprocess obtain its own view of that
single owner. The view shape is chosen at the registration site by the
optional `worker = …` argument; see the
[Worker view subsection](#worker-view-descriptor-rpctrait-bothtrait)
for the full picker. The default — and the only one this top-of-section
example uses — is `worker = descriptor`: implement the
[`HostedDep`](https://docs.rs/test-r/latest/test_r/core/trait.HostedDep.html)
trait for the dependency type, then annotate the constructor with
`scope = Hosted`:

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
- With `worker = descriptor` (the default), the owner type and the
  worker handle type share the same Rust type (`Self`). The
  implementor is responsible for keeping owner-only fields (sockets,
  accept loops, container handles) in `Option`s or `Arc`s that workers
  don't populate. The `worker = rpc(Trait)` and `worker = both(Trait)`
  views generate a separate `<Trait>Stub` for the RPC side, so this
  invariant only applies to the descriptor view.

### Worker view: `descriptor`, `rpc(Trait)`, `both(Trait)`

`scope = Hosted` accepts an optional `worker = …` argument on
`#[test_dep]` that chooses **what shape the worker subprocess sees**
for the same parent-held owner:

| `worker = …`         | Worker-visible value                                                                                              | Owner trait(s) required                                                  |
|----------------------|-------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------|
| `descriptor` (default) | `HostedDep::from_descriptor(parent_descriptor_bytes)` — a reconstructed handle (typically holding an address). | [`HostedDep`](https://docs.rs/test-r/latest/test_r/core/trait.HostedDep.html) (or [`AsyncHostedDep`](https://docs.rs/test-r/latest/test_r/core/trait.AsyncHostedDep.html)). |
| `rpc(Trait)`         | An auto-generated `<Trait>Stub` whose methods route each call back to the parent over the runtime's IPC channel. | [`HostedRpcDep`](https://docs.rs/test-r/latest/test_r/core/trait.HostedRpcDep.html) implemented for the owner; trait declared with [`#[hosted_rpc]`](#hostedrpc-attribute-macro-eliminating-the-boilerplate). |
| `both(Trait)`        | **Both** a descriptor-shaped handle and a `<Trait>Stub`, backed by the same single owner instance.                | Descriptor side: [`HostedDep`](https://docs.rs/test-r/latest/test_r/core/trait.HostedDep.html) **or** [`AsyncHostedDep`](https://docs.rs/test-r/latest/test_r/core/trait.AsyncHostedDep.html). RPC side: [`HostedRpcDep`](https://docs.rs/test-r/latest/test_r/core/trait.HostedRpcDep.html). Both impls are on the same owner type. |

In all three cases the parent constructs the owner exactly once and
keeps it alive for the whole suite. Workers obtain their view through
the existing IPC channel; nothing changes about parallelism or output
capturing relative to the default Hosted strategy.

- `worker = descriptor` is the implicit default. The historical
  `#[test_dep(scope = Hosted)]` syntax stays equivalent to
  `#[test_dep(scope = Hosted, worker = descriptor)]`.
- `worker = rpc(Trait)` is the HR3.2.0 sugar for what used to be a
  separate `#[test_dep(scope = HostedRpc, stub = <StubType>)]`
  registration. See the
  [`#[hosted_rpc]` attribute macro section](#hostedrpc-attribute-macro-eliminating-the-boilerplate)
  for the trait-side machinery; the only difference at the registration
  site is which scope you use.
- `worker = both(Trait)` is the HR3.2.0 way to share a singleton that
  needs to expose **both** a bulk-data descriptor handle (typically a
  connection address used by a gRPC client) **and** a small RPC control
  surface (kill / flush / snapshot) from a single owner. Tests can
  parameterise on either the descriptor type, the auto-generated
  `<Trait>Stub`, or both; no duplicate owner is constructed.

`async_worker` is no longer needed on any of these forms — the runtime
picks the sync vs async worker-side reconstructor automatically based
on the active runtime (sync vs `tokio`) and the dep's
`HostedDep` / `AsyncHostedDep` implementation. The flag still parses
for source compatibility but emits a `#[deprecated]` warning at the
registration site.

### Async worker-side reconstruction (`AsyncHostedDep`)

The default `HostedDep::from_descriptor` is synchronous. When worker-side
reconstruction needs to `.await` — opening async network clients,
calling async constructors of downstream services such as
`ProvidedWorkerService::new(...).await` — implement
[`AsyncHostedDep`](https://docs.rs/test-r/latest/test_r/core/trait.AsyncHostedDep.html)
instead. Under the `tokio` runtime feature the runtime automatically
drives every Hosted reconstruction through the async path, so no
extra `#[test_dep]` flag is required:

```rust
use test_r::core::AsyncHostedDep;
use test_r::{test, test_dep};

impl AsyncHostedDep for LiveAsyncService {
    fn descriptor(&self) -> Vec<u8> {
        self.addr.to_string().into_bytes()
    }

    async fn from_descriptor(bytes: &[u8]) -> Self {
        let s = std::str::from_utf8(bytes).expect("utf-8");
        let addr: SocketAddr = s.parse().expect("addr");
        // Async work only legal because `from_descriptor` is async on
        // `AsyncHostedDep`. The sync `HostedDep` flavour could not do this.
        let stream = TcpStream::connect(addr).await.expect("connect");
        Self { addr, prewarmed_client: Some(stream) }
    }
}

#[test_dep(scope = Hosted)]
async fn live_async_service() -> LiveAsyncService {
    LiveAsyncService::new().await
}
```

Semantics are otherwise identical to plain `HostedDep`:

- The parent still constructs the owner exactly once and ships
  `descriptor()` bytes to every worker.
- Each worker runs the async `from_descriptor` inside a
  `WorkerReconstructor::Async` closure on its tokio runtime.
- The blanket `impl<T: HostedDep> AsyncHostedDep for T` makes a sync
  `HostedDep` implementation usable through the async path too, so
  switching the consumer crate to the `tokio` feature does not require
  rewriting existing sync implementations.

> **Note — `async_worker` is deprecated.** The
> `#[test_dep(scope = Hosted, async_worker)]` attribute flag from
> earlier releases is no longer needed: the macro now selects the
> worker-side reconstruction path purely from the active runtime
> (sync vs `tokio`) and the dep's `HostedDep` / `AsyncHostedDep`
> implementation. The flag still parses for source compatibility but
> emits a compile-time `#[deprecated]` warning at the registration site.

The
[`hosted_async_worker` example](https://github.com/vigoo/test-r/blob/main/example-tokio/src/sharing/hosted_async_worker.rs)
demonstrates the full pattern, including a regression test that confirms
the worker-side reconstructor actually runs only in worker subprocesses.

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

> **Preferred registration syntax — `scope = Hosted` with
> `worker = rpc(Trait)`.** Since the HR3.2.0 worker-view picker was
> added, the recommended way to register an RPC-shaped Hosted dep
> backed by a trait is
> `#[test_dep(scope = Hosted, worker = rpc(<Trait>))]`. That syntax
> uses the same runtime mechanism documented in this section but
> drops the explicit `stub = <StubType>` argument: the macro derives
> the worker-visible stub type from the trait name and writes the
> registration entry for you. Use it together with the
> [`#[hosted_rpc]`](#hostedrpc-attribute-macro-eliminating-the-boilerplate)
> attribute macro on a normal Rust trait.
>
> The legacy `#[test_dep(scope = HostedRpc, stub = <StubType>)]`
> form remains supported and continues to be the right choice when
> there is no Rust trait surface — for example when you ship a
> hand-written stub with custom method indices, custom argument
> framing, or no trait declaration at all. The two
> [`hosted_rpc_basic`](https://github.com/vigoo/test-r/blob/main/example/src/sharing/hosted_rpc_basic.rs)
> examples are intentionally kept on the legacy form for that reason.

Use this when:

- the dep is a singleton (an id allocator, a leadership coordinator, a
  globally-shared counter, …),
- you don't want to invent and embed a real network protocol for tests,
- a few hundred call-per-test of overhead per RPC are acceptable
  (every call is one synchronous round-trip on the existing IPC socket).

### MVP scope

- Both runners are supported. The sync runner shipped in Phase 1C and
  the tokio runner shipped in Phase HR1.2; tests in both runners see
  the same `Stub` value and call into the same parent-held owner via
  the IPC transport (or `InProcessHostedRpcTransport` in the
  `--nocapture` / no-spawn-workers path).
- The user implements the owner type, the worker-visible stub type, and
  one method-dispatch function on the owner. The runtime wires those
  together over IPC. For trait-shaped owners, the
  [`#[hosted_rpc]` attribute macro](#hostedrpc-attribute-macro-eliminating-the-boilerplate)
  generates the stub struct, the per-method `desert_rust` encode/decode
  shims and the dispatch arms for you.
- One in-flight call at a time per worker subprocess. Each `stub.foo()`
  takes the IPC connection lock, writes the request frame, reads exactly
  one reply frame, and returns. No multiplexer or out-of-order replies.
- The stub methods are **synchronous** even under the tokio runner: the
  tokio transport bridges the sync trait method to the async IPC
  primitives via `tokio::task::block_in_place` +
  `Handle::current().block_on(...)`. Native async stub methods are
  deferred.

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

### `#[hosted_rpc]` attribute macro: eliminating the boilerplate

Writing the `LastUniqueIdStub` struct, the per-method argument
serialisation and the `match method_idx { ... }` arm in
`HostedRpcDep::dispatch` is mechanical work. The `#[hosted_rpc]`
attribute macro generates all of it from a user trait declaration.

```rust
use test_r::core::{HostedRpcChannel, HostedRpcDep};
use test_r::{hosted_rpc, test, test_dep};

#[hosted_rpc]
pub trait Counter {
    fn next(&self) -> u64;
    fn reserve(&self, count: u32) -> u64;
    fn echo(&self, msg: String) -> String;
}

pub struct CounterOwner { counter: std::sync::Mutex<u64> }

impl Counter for CounterOwner {
    fn next(&self) -> u64 {
        let mut g = self.counter.lock().unwrap();
        *g += 1; *g
    }
    fn reserve(&self, count: u32) -> u64 {
        let mut g = self.counter.lock().unwrap();
        let first = *g + 1; *g += count as u64; first
    }
    fn echo(&self, msg: String) -> String { msg }
}

impl HostedRpcDep for CounterOwner {
    type Stub = CounterStub;
    fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
        // Generated by `#[hosted_rpc]`. Routes the wire `method_idx` to
        // the matching method on `self`, decoding args / encoding the
        // reply with `desert_rust`.
        CounterDispatch::dispatch_counter(self, method_idx, args)
    }
    fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
        // Generated by `#[hosted_rpc]`. Wraps the `HostedRpcChannel` in
        // the worker-side stub that implements `Counter`.
        CounterStub::new(channel)
    }
}

#[test_dep(scope = Hosted, worker = rpc(Counter))]
fn counter_owner() -> CounterOwner {
    CounterOwner { counter: std::sync::Mutex::new(0) }
}

#[test]
fn ids_are_monotonic(c: &CounterStub) {
    let a = c.next();
    let b = c.next();
    assert!(a < b);
}
```

`scope = Hosted, worker = rpc(Counter)` is the preferred HR3.2.0
registration form for trait-shaped owners. The macro derives the
worker-visible stub type from the trait name (`Counter` → `CounterStub`)
so you do not need to pass it explicitly. The equivalent legacy form
`#[test_dep(scope = HostedRpc, stub = CounterStub)]` still works and
remains the right choice for hand-written stubs that are not backed by
a trait at all (see the [`HostedRpcDep`](#hostedrpcdep-the-trait)
example above).

What the macro emits next to the trait declaration:

- A struct `<Trait>Stub { channel: HostedRpcChannel }` with a
  `pub fn new(channel) -> Self` constructor and an `impl <Trait> for
  <Trait>Stub` that implements every trait method by encoding the args
  as a tuple of the parameter types (1-arg methods send the bare value;
  0-arg methods send `()`; 2+-arg methods send a regular tuple), shipping
  them through `HostedRpcChannel::call(method_idx, ...)` and decoding the
  return value. Encoding uses `desert_rust`.
- A trait `<Trait>Dispatch` with a single method
  `dispatch_<snake_case_trait_name>(&mut self, method_idx: u32, args: &[u8])
  -> Result<Vec<u8>, String>`, **blanket-implemented for every
  `T: <Trait>`**, that contains the per-method match arms wiring incoming
  RPCs back to the owner's `<Trait>` impl. The owner's
  `HostedRpcDep::dispatch` becomes a one-line delegation.

Wire-format details:

- Method indices are assigned by source order in the trait, starting at
  `0`, and shipped on the wire as `u32`. Reordering the methods is a
  breaking change.
- Args are encoded with `desert_rust::serialize_to_byte_vec` as a tuple
  of the parameter types after stripping `self`. The zero-arg case uses
  `()`; the single-arg case uses the bare `T` (NOT a 1-tuple) so the
  framing stays symmetric on the dispatch side.
- The return value is encoded directly. The unit return type uses `()`.

MVP restrictions enforced at macro time (the macro emits a
`compile_error!` if violated):

- `#[hosted_rpc]` does not take any attribute arguments
  (`#[hosted_rpc(...)]`).
- The trait must be non-generic, must not be `unsafe trait`, must not
  have supertraits, and must only declare methods (no associated
  `type` / `const` items).
- Methods must be non-generic, synchronous (no `async fn`), must not
  be `unsafe fn`, must use the default Rust ABI (no `extern "..."`),
  must not be variadic, must not have a default body, and the first
  argument must be **`&self`** (no by-value `self`, no explicit
  `self: T` type, and **no `&mut self`** either — test-r injects test
  deps as `&Stub` immutable references, so `&mut self` stub methods
  would compile but be uncallable from a normal
  `#[test] fn (s: &MyStub)` parameter).
- Argument types must use plain identifier patterns (no `_`, no
  destructuring like `(a, b): (u32, u32)`).
- `impl Trait` is not allowed in argument or return position.
- `#[cfg(...)]` / `#[cfg_attr(...)]` are not allowed on the trait or
  its methods (the generated sibling items and dispatch arms are not
  cfg-propagated in the MVP).

All arg and return types must implement `desert_rust::BinarySerializer`
and `desert_rust::BinaryDeserializer`. Common standard-library types
(`u8`/`u16`/`u32`/`u64`/`i*`, `bool`, `String`, `Vec<T>`, `Option<T>`,
`HashMap<K, V>` and N-ary tuples for `N >= 2`) already do.

Transport, codec and dispatch failures (IPC errors, owner panics,
encode/decode errors) **panic in the generated stub** with an
`expect(...)` message of the form `hosted_rpc(<Trait>::<method>): ...`.
User-level errors are still encoded normally: if a trait method
returns `Result<T, E>`, the `Result` itself is shipped over the wire
and only infrastructure failures panic.

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

- The owner constructor must be **synchronous** in the MVP, on both the
  sync and tokio runners (no `async fn` constructors).
- The stub trait methods are synchronous on both runners. Async stub
  methods are deferred.
- The constructor must not take other `#[test_dep]` parameters (mirrors
  `Hosted`).
- The constructor must return the **owner type**; tests must parameterise
  on the **stub type** named via `stub = StubType`.
- One in-flight RPC at a time per worker subprocess. Pipelined or
  concurrent calls are not supported in the MVP.
- The tokio HostedRpc transport relies on the runner being driven by a
  multi-thread tokio runtime so `tokio::task::block_in_place` /
  `Handle::current().block_on(...)` can re-enter the IPC I/O from the
  sync stub trait method. The built-in test-r tokio runner satisfies
  this; a custom runner on a `current_thread` runtime is not supported.

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
