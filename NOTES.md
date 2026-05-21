# Implementation Plan: Per-Dependency Scoping for test-r

> **Status:**
> - **Phase 1A — DONE.** `PerWorker` + `Cloneable` ship end-to-end in test-r
>   (sync + tokio), with macro support, IPC wiring, examples, unit tests,
>   integration coverage via the example crates, and a new book chapter.
>   Cloneable dep identity is now keyed by fully-qualified id
>   (`{crate}::{module}::{name}`) end-to-end so same-named deps registered in
>   different modules cannot collide on the wire.
>   See "Phase 1A exit criteria" below for details.
> - **Phase 1B — DONE.** `Hosted` ships in test-r using a **parent-hosted**
>   design (not the originally-planned sidecar dep-host process). The parent
>   test runner materialises every `Hosted` owner exactly once and keeps it
>   alive for the duration of the suite, then ships descriptor bytes
>   (`HostedDep::descriptor`) to each spawned worker over the same IPC path
>   used for Cloneable. Each worker reconstructs a per-worker handle via
>   `HostedDep::from_descriptor`. Hosted does **not** trigger the
>   single-thread fallback under capture — parallel workers are preserved.
>   See "Phase 1B exit criteria" below for details.
> - **Phase 1C — DONE (MVP).** `HostedRpc` ships as an MVP in test-r's sync
>   runner. It deliberately covers only manual-owner + manual-stub +
>   serialized synchronous calls; tokio support, the `#[hosted_rpc]` trait
>   macro, and the worker-side reader / multiplexer are deferred.
>   Oracle reviewed the implementation post-merge and confirmed the MVP
>   semantics are sound provided the **MVP temporal invariant** is upheld
>   (stub calls only happen from inside a running test body; `build_stub`
>   is cheap/side-effect-free and never calls back into the channel). That
>   invariant is now codified in `HostedRpcDep` rustdoc and in the
>   "MVP temporal invariant" subsection of the dependency-sharing book
>   chapter. An end-to-end IPC dispatch-error regression test
>   (`hosted_rpc_unknown_method_surfaces_as_dispatch_error`) sits in
>   `example/src/sharing/hosted_rpc_basic.rs` alongside the in-process
>   unit tests in `test-r-core`.
>   See "Phase 1C — shipped (MVP) — DONE" below for details. Any
>   `#[hosted_rpc]` trait macro / multiplexer / tokio material that appears
>   later in this document (HR1/HR2/HR3 sections) is explicitly **future /
>   deferred** work and is NOT part of the shipped MVP.
> - Phase 2, 3 — not started.

## Background

Today test-r forces single-threaded execution whenever output capturing is on AND
any test-dep exists, because:
- Capturing requires one worker child process per thread (the parent reads child
  stdout/stderr pipes to attribute output to tests).
- A child process cannot receive `Arc<dyn Any + Send + Sync>` values held in the
  parent's materialized dependencies.

This plan lifts that restriction by giving each `#[test_dep]` an explicit
**sharing strategy**:

| Strategy   | What it means                                                                                      |
|------------|----------------------------------------------------------------------------------------------------|
| `Shared`   | (default, today's behaviour) — one materialized instance, forces single-threaded when capturing.   |
| `PerWorker`| Each worker child materializes its own instance. Tests within one worker share it.                 |
| `Cloneable`| Parent builds once; ships wire bytes to each worker; each worker reconstructs a local value.       |
| `Hosted`   | The top-level parent process owns the value; parent distributes a small descriptor to each worker. |

`Cloneable` and `Hosted` both use a pair of functions: the **owner** constructor
runs exactly once in the top-level parent process, and a separate **worker
reconstructor** runs in each worker. The worker reconstructor can take other
worker-local deps as parameters (this is what lets, e.g., a precompiled wasm
component be re-hydrated against a worker-local `wasmtime::Engine`).

For `Hosted`, the owner and the worker handle share the same Rust type (`Self`).
The implementor keeps owner-only state (sockets, accept loops, container
handles) in `Option`s/`Arc`s that worker handles don't populate, and exposes
the addressable bits via `HostedDep::descriptor` /
`HostedDep::from_descriptor`.

> Many of the section-level subheadings below (1A.x, 1B.x, etc.) were
> originally written against the pre-pivot **sidecar dep-host process**
> design. The shipped Phase 1B model is the simpler **parent-hosted**
> design described above and in the section "1B.6 — Mode-consistent Hosted
> semantics" and "1B.7 — Parent-only Hosted-owner materialisation" below.
> Where older text contradicts the parent-hosted shipped model, the
> shipped model wins.

---

## Phase 1A — test-r core: `PerWorker` + `Cloneable`

The goal of 1A is to ship the two simpler scopes, prove the new metadata model
end-to-end in a real codebase (Phase 2), and defer the dep-host sidecar to 1B.

### 1A.0 — Prerequisite: fix IPC framing

`IpcCommand`/`IpcResponse` payloads are currently length-prefixed with a `u16`
(`sync.rs` and `tokio.rs`). A precompiled wasm component or a parsed-WIT
descriptor is easily megabytes. Before any `Cloneable` work:

- Change framing to `u32` length prefix.
- Extract a shared `write_framed` / `read_framed` helper used by parent, worker,
  and (later) dep host.
- Add a unit test that round-trips a payload > 64 KiB.

### 1A.1 — Specification & design

**Deliverable:** `book/src/design/sharing-strategy.md` covering the model,
attribute syntax, runtime architecture, IPC additions, and graph rules.

Locked-in decisions:

1. **Attribute syntax**

   ```rust
   #[test_dep]                                                  // = Shared (today)
   #[test_dep(scope = Shared)]                                  // explicit
   #[test_dep(scope = PerWorker)]
   #[test_dep(scope = Cloneable)]                               // Phase 1A — see note below
   #[test_dep(scope = Hosted,    client = cluster_client)]      // Phase 1B
   ```

   > **As-shipped note (Phase 1A):** `#[test_dep(scope = Cloneable)]` does
   > **not** accept a `worker = ...` override. The runtime auto-derives the
   > worker reconstructor from the dep's `CloneableDep` impl, so the wire
   > payload IS the dep value. The macro rejects `worker = ...` at compile
   > time and reserves it for Phase 1B (`scope = Hosted`).

   `tagged_as = "..."` continues to work orthogonally.

   The `worker` / `client` function is **not** itself a `#[test_dep]`. It is a
   plain `fn`/`async fn` that the macro records into a parallel "reconstructor
   registry" keyed by the owner dep's identity. This keeps the existing
   "one `#[test_dep]` = one registered dep keyed by return type" invariant.

2. **Special parameter types** (only allowed on worker/client functions):

   - `Wire<T>` — the deserialized wire payload from the parent (Cloneable).
   - `Descriptor<T>` — the descriptor produced by a Hosted owner.

   These are recognised structurally in the macro (path-suffix match on
   `Wire`/`Descriptor` in the type position), not by trait inference.

3. **Traits**

   ```rust
   pub trait CloneableDep: Sized + Send + Sync + 'static {
       type Wire: Serialize + DeserializeOwned + Send + 'static;
       fn to_wire(&self) -> Self::Wire;
   }

   pub trait HostedDep: Sized + Send + Sync + 'static {
       type Descriptor: Serialize + DeserializeOwned + Send + 'static;
       fn descriptor(&self) -> Self::Descriptor;
   }
   ```

   No auto-derive in 1A; explicit impls only. (A `#[derive(CloneableDep)]` that
   uses `serde`'s round-trip as the wire form can come later.)

4. **Two-locus graph rules**

   Every `#[test_dep]` now has up to two constructors that execute in different
   places. Each has its own visibility scope:

   | Locus            | May depend on                                                |
   |------------------|--------------------------------------------------------------|
   | Parent owner     | Other parent-visible deps (Shared, Cloneable owner, Hosted owner). |
   | Worker reconstructor | Worker-visible deps (PerWorker, Cloneable worker, Hosted client). |
   | Worker test body | Worker-visible deps only.                                    |

   Crossing the boundary (e.g., a `PerWorker` dep used by a parent-side owner
   constructor) is a compile-time error in the macro via the registered
   metadata.

   Stored metadata per dep:
   ```rust
   struct DepMetadata {
       scope: DepScope,                         // Shared | PerWorker | Cloneable | Hosted
       owner_deps: Vec<DepRef>,                 // params of the parent/host fn
       worker_deps: Vec<DepRef>,                // params of the worker fn (if any)
       worker_fn: Option<&'static WorkerFn>,    // reconstructor pointer
   }
   ```

5. **Scope-mixing rules** for `finalize_for_execution`:

   - All deps are `PerWorker` / `Cloneable` → unrestricted parallelism, no host.
   - At least one `Shared` in scope → today's fallback (single-threaded with
     spawn-workers when capturing).
   - At least one `Hosted` (Phase 1B) → spawn dep host, parallel workers OK.
   - `Hosted` + `Shared` in scope → still parallel; the dep host materialises
     both. (A `Shared` dep in this case is just a `Hosted` dep with no
     descriptor distribution — no test needs a worker handle to it.)

### 1A.2 — `#[test_dep]` macro changes (test-r-macro)

- Extend the `darling`-parsed attribute struct with
  `scope: Option<Scope>`, `worker: Option<Path>`.
- Default scope is `Shared`. **As shipped (Phase 1A):** the macro rejects
  `worker = ...` for every scope — it is reserved for Phase 1B (`scope =
  Hosted`). Phase 1A `scope = Cloneable` deps auto-derive their worker
  reconstructor from `CloneableDep`. The macro also rejects Cloneable deps
  that take other deps as constructor parameters (Phase 1A owners run on the
  parent with an empty dep view).
- Inspect parameter types of the worker fn: if any is `Wire<T>`, ensure the
  owner dep's return type is `T`. Other params are recorded as worker deps.
- Emit a `static DEP_METADATA: DepMetadata = …` near each generated registration
  so the runtime can read scope/locus info without proc-macro tricks.
- Add `trybuild` compile-fail tests for: missing `worker`, `worker` on Shared,
  worker dep used by an owner fn, return-type mismatch in `Wire<T>`.
- Snapshot tests for the generated registration code for each scope.

### 1A.3 — `RegisteredDependency` surface (test-r-core/internal)

- Add `scope: DepScope` and `worker_fn: Option<…>` fields. These are part of the
  public `internal` surface today.
- Treat this as a deliberate **breaking change**: bump test-r to the next minor
  version (or major, depending on current versioning policy). Provide a
  `RegisteredDependency::new_shared(...)` constructor that downstream code
  using the public surface can call to retain old behaviour.
- Document the break in `CHANGELOG.md`.

### 1A.4 — `PerWorker` runtime support

The smallest change in this phase.

- `TestExecution::has_shared_dependencies(&self) -> bool` checks only deps with
  `DepScope::Shared`; `finalize_for_execution` uses that for the single-thread
  fallback.
- `skip_creating_dependencies` becomes "skip in parent any dep that is not
  Shared/Hosted" — workers materialise PerWorker themselves on first use, the
  same way `spawn_workers` already pushes materialisation into the child.
- No new IPC.

Tests:
- `example/`: a `PerWorker` `TempDir` dep, four tests, capture on, assert all
  four run with `--test-threads 4` (timing-based assertion via a barrier) and
  each receives a distinct path.

### 1A.5 — `Cloneable` runtime support

- New IPC messages (as-shipped: keyed by the dep's fully-qualified id
  `{crate}::{module}::{name}` so same-named deps in different modules cannot
  collide on the wire):
  ```rust
  IpcCommand::ProvideCloneable { dep_id: String, wire_bytes: Vec<u8> }
  IpcResponse::CloneableAccepted { dep_id: String }
  ```
- Parent runs the owner fn exactly once per Cloneable dep, calls `to_wire()`,
  serialises with the existing `desert-rust` codec, caches the bytes.
- Before each worker's first test that needs the dep, parent sends
  `ProvideCloneable`. Workers buffer the bytes in their own dependency map.
- When `pick_next` resolves a test, the worker's dep-materialisation path sees
  the dep is Cloneable and that wire bytes are present, then calls the
  registered worker fn passing `Wire(bytes_deserialised_to_T::Wire)` plus any
  other worker deps.
- `to_wire` errors / panics in the parent terminate the suite cleanly with a
  descriptive error.

Tests:
- A Cloneable dep whose wire is `Vec<u8>`. The owner fn increments a static
  `AtomicUsize`; assertion: the counter is 1 even with `--test-threads 4`.
- Compile-fail: worker fn missing `Wire<T>` parameter.

### 1A.6 — Argument-parsing / runtime wiring (1A scope)

- `Arguments::finalize_for_execution` only consults `has_shared_dependencies()`.
- No new CLI args yet (those come in 1B for `--dep-host`).

### 1A.7 — Documentation (1A scope)

- New chapter "Dependency sharing strategies" in `book/`, covering Shared,
  PerWorker, Cloneable. A "Hosted: coming soon" stub points forward to 1B.
- Updated "Output capturing" chapter explaining when parallelism is preserved.
- Migration note: unannotated `#[test_dep]` keeps working unchanged.

### 1A.8 — Example crates (1A scope)

- `example/` and `example-tokio/` each gain a `per_worker_basic.rs` and
  `cloneable_basic.rs` test module. The examples compile in CI and double as
  smoke tests.

### 1A.9 — Integration tests (1A scope)

- New `test-r-integration-tests/` workspace member runs the example test
  binaries as subprocesses with various flag combinations, asserting both the
  pass/fail outcome and observable parallelism (timestamps in captured output).

### Phase 1A exit criteria — status

| Criterion                                                                   | Status |
|-----------------------------------------------------------------------------|--------|
| `PerWorker` and `Cloneable` implemented behind opt-in attributes.           | ✅      |
| Unannotated deps still take today's `Shared` path bit-for-bit.              | ✅      |
| IPC framing widened to `u32`, payloads > 64 KiB tested.                     | ✅      |
| Cloneable wire/runtime keyed by fully-qualified id (oracle identity bug).   | ✅ (regression test in `cloneable_tests::cloneable_value_routing_uses_qualified_id_across_modules`) |
| Tokio runner awaits `async fn` Cloneable owner constructors on the parent.  | ✅ (`collect_cloneable_wire_bytes_async`; unit test `async_cloneable_wire_collection_awaits_async_constructor`; tokio example uses `async fn create_payload`) |
| Macro rejects Cloneable owner constructors that take other deps as params.  | ✅      |
| Macro rejects `worker = ...` (reserved for Phase 1B `Hosted`).              | ✅      |
| `cargo test` green for the new sharing tests under capture + N workers.     | ✅      |
| `cargo clippy --no-deps --all-targets -- -Dwarnings` clean.                 | ✅      |
| `cargo fmt --all -- --check` clean.                                         | ✅      |
| Sharing-strategy chapter in `book/`.                                        | ✅      |
| `mdbook build book` succeeds.                                               | ✅      |
| Compile-fail / trybuild tests for all macro misuse cases.                   | ⏳ (deferred — runtime panics still cover the misuse paths) |

Key landing surface (for the next phase to build on):

- `test_r_core::internal::DepScope`, `CloneableDep`, `CloneableCodec`, `WorkerReconstructor`, `RegisteredDependency::{scope, worker_fn, cloneable_codec}`.
- `test_r_core::ipc::{write_frame, read_frame, write_frame_async, read_frame_async}` + `IpcCommand::ProvideCloneable` / `IpcResponse::CloneableAccepted`.
- `TestSuiteExecution::collect_cloneable_wire_bytes_sync` and `provide_cloneable_value`.
- Macro: `#[test_dep(scope = PerWorker)]`, `#[test_dep(scope = Cloneable)]` (both bare-ident and string-literal forms accepted; `Hosted` reserved for 1B with a clear macro error; `worker = ...` is rejected at compile time in Phase 1A — Cloneable deps auto-derive their worker reconstructor from `CloneableDep`).
- Sync runner: parent computes Cloneable wire bytes once, ships to each spawned worker via IPC; workers stash pre-resolved values in the execution tree so `materialize_deps_sync` skips re-running the constructor.
- Tokio runner: equivalent async path.
- Examples: `example/src/sharing/{per_worker_basic,cloneable_basic}.rs` and `example-tokio/src/sharing/{per_worker_basic,cloneable_basic}.rs`.
- Unit tests: `test-r-core/src/execution.rs` (`cloneable_tests` module) — cover wire-collection and worker-side pre-population paths.
- Docs: `book/src/advanced_features/dependency_sharing.md` linked from `SUMMARY.md`.

---

## Phase 1B — test-r core: `Hosted` (parent-hosted, as shipped)

The originally-planned design used a separate **dep-host sidecar process**:
the parent would spawn a dep host, the dep host would materialise every
`Hosted` owner, the parent would query each `descriptor()` over IPC, and the
parent would then distribute descriptors to workers.

**Design pivot during implementation:** the user's only hard requirement was
that env/service deps be **hosted exactly once** (not duplicated per worker).
The dep host sidecar adds a second IPC topology, a second supervised
subprocess, and a third lifetime to coordinate (parent / dep host / workers)
— none of which is needed to satisfy the "host once" requirement. We
therefore shipped a **parent-hosted** model instead: the parent test runner
materialises every Hosted owner once and keeps it alive itself.

### 1B.1 — As-shipped process model

```diagram
╭────────╮   IPC (ProvideHostedDescriptor)   ╭─────────╮
│ Parent │───────────────────────────────────▶│ Worker  │
│        │                                   ╰─────────╯
│ holds  │   IPC (ProvideHostedDescriptor)   ╭─────────╮
│ Hosted │───────────────────────────────────▶│ Worker  │
│ owners │                                   ╰─────────╯
╰────────╯
   ▲
   │ workers reach owner directly (TCP, gRPC, …)
   ╰─── back-channel determined by the descriptor ──
```

- Parent runs the owner constructor **exactly once** per Hosted dep at
  startup, via `TestSuiteExecution::collect_hosted_descriptor_bytes_sync`
  (or `_async` under tokio).
- Parent stashes the owner values in `_hosted_owners` (a `Vec<Arc<dyn Any +
  Send + Sync>>`) for the duration of the suite — they outlive every worker.
- Parent encodes each owner via `HostedDep::descriptor()` and ships the
  bytes to every worker over IPC (`IpcCommand::ProvideHostedDescriptor`,
  reusing the same framing as `ProvideCloneable`).
- Each worker calls `HostedDep::from_descriptor(bytes)` (via the registered
  worker reconstructor) to materialise a per-worker handle.
- The actual data plane (TCP, gRPC, whatever) is determined entirely by the
  descriptor: workers connect directly to the owner — there is no
  test-r-mediated RPC between worker and owner.

### 1B.2 — Public API surface

- `pub trait HostedDep` in `test_r_core::internal` (re-exported from
  `test_r::core`):
  ```rust
  pub trait HostedDep: Sized + Send + Sync + 'static {
      fn descriptor(&self) -> Vec<u8>;
      fn from_descriptor(bytes: &[u8]) -> Self;
  }
  ```
  Like `CloneableDep`, the wire encoding is opaque to the runner.
- `DepScope::Hosted` variant. `requires_single_thread_when_capturing()`
  returns **false** for Hosted — Hosted is parallel-safe under capture.
- `RegisteredDependency::hosted_codec: Option<CloneableCodec>` (parallel to
  `cloneable_codec`; the two are mutually exclusive — the runtime dispatches
  on whichever is populated).

### 1B.3 — Macro support

- `#[test_dep(scope = Hosted)]` is parsed and accepted by the macro.
- The macro auto-generates the `hosted_codec` (from `HostedDep::descriptor`)
  and the worker reconstructor (`HostedDep::from_descriptor`).
- The macro rejects:
  - Hosted owners that take other `#[test_dep]` parameters (matches the
    Phase 1A Cloneable restriction; owner-side dependency wiring is
    reserved for a future phase).
  - `worker = ...` on any scope (reserved for future use).

### 1B.4 — IPC

- `IpcCommand::ProvideHostedDescriptor { dep_id, wire_bytes }` — same shape
  as `ProvideCloneable`, keyed by the fully-qualified dep id
  (`{crate}::{module}::{name}`).
- `IpcResponse::HostedDescriptorAccepted { dep_id }`.
- No separate dep-host IPC topology. The sidecar-only variants
  (`GetDescriptor`, `ShutdownDepHost`, `DescriptorReady`, `DepHostShutdown`,
  `--dep-host` arg) that briefly appeared during development have been
  removed.

### 1B.5 — Capture / parallelism rules

- `TestSuiteExecution::has_hosted_dependencies()` is exposed.
- `Arguments::finalize_for_execution` checks only `has_shared_dependencies()`
  for the single-thread fallback. Hosted deps do **not** force the fallback.

### 1B.6 — Mode-consistent Hosted semantics

The parent test runner ALWAYS materialises Hosted owners and keeps them
alive in `_hosted_owners` for the duration of the suite, regardless of
whether it spawns worker processes:

- **Spawn-workers mode (capture on, parallel)**: parent collects descriptors
  once, ships them to every worker via `ProvideHostedDescriptor`, workers
  call `HostedDep::from_descriptor` to materialise per-worker handles.
- **No-spawn-workers mode (`--nocapture`, single-process)**: parent
  collects descriptors once, then calls `apply_hosted_descriptors_locally`
  to reconstruct the same `HostedDep::from_descriptor` handles directly
  on the parent and pre-populate the execution tree so tests see the
  same handle type regardless of execution mode.

This was an oracle-suggested fix during Phase 1B review to avoid a footgun
where tests would silently receive the raw owner value (instead of the
worker-side handle) when run without capture.

### 1B.7 — Parent-only Hosted-owner materialisation (oracle second-pass)

The second oracle review pointed out that the initial Phase 1B
implementation collected Hosted owner descriptors whenever
`execution.has_hosted_dependencies()` was true — without checking whether
the current process was the top-level parent or an IPC worker subprocess.

The fix:

- `Arguments::is_top_level_parent(&self) -> bool` is a new helper that
  returns `true` iff `--ipc <name>` was NOT set on the command line.
- Both `sync.rs` and `tokio.rs` now gate `collect_hosted_descriptor_bytes_*`
  (and the local-apply fallback) on `is_top_level_parent && …`.
- Same gating is also applied to `collect_cloneable_wire_bytes_*` for
  consistency.
- IPC worker subprocesses now do nothing on startup except wait for
  `ProvideHostedDescriptor` from the parent and call
  `HostedDep::from_descriptor` on the received bytes.

Why this matters:

- Hosted owners are typically singletons (TCP listeners, Docker
  containers, env-based test environments). Running them in every worker
  would duplicate the resource and cause port/container/env conflicts.
- If a Hosted constructor panicked before the worker accepted the parent's
  IPC connection, the parent would block in `accept`. The parent-only
  gating eliminates that hang mode entirely.

Regression coverage:

- `Arguments::is_top_level_parent_when_ipc_unset` and
  `Arguments::ipc_worker_is_not_top_level_parent` unit tests in
  `test-r-core/src/args.rs` lock the gating helper.
- `hosted_owner_runs_only_in_top_level_parent` integration tests in
  `example/src/sharing/hosted_basic.rs` and
  `example-tokio/src/sharing/hosted_basic.rs` use
  `std::env::args().any(|a| a == "--ipc")` to detect IPC-worker mode and
  assert the in-process `OWNER_CTOR_RUNS` counter is `0` in workers and
  `1` in the parent. Manually verified this test ALSO fails when the
  guard is removed.

### Phase 1B exit criteria — status

| Criterion                                                                       | Status |
|---------------------------------------------------------------------------------|--------|
| `HostedDep` trait + `DepScope::Hosted` shipped, re-exported from `test_r::core` | ✅      |
| Macro accepts `#[test_dep(scope = Hosted)]` and auto-derives codecs             | ✅      |
| Macro rejects Hosted owners that take other test-deps                           | ✅      |
| Parent materialises Hosted owners exactly once and keeps them alive             | ✅      |
| Parent ships descriptors to every worker over IPC                               | ✅      |
| Workers reconstruct handles via `HostedDep::from_descriptor`                    | ✅      |
| Mode-consistent: `--nocapture` path also goes through `from_descriptor`         | ✅ (regression test `hosted_no_spawn_workers_uses_worker_side_handle`) |
| **IPC workers do NOT run Hosted owner constructors** (parent-only gating)       | ✅ (regression tests `Arguments::is_top_level_parent` unit tests + `hosted_owner_runs_only_in_top_level_parent` integration test) |
| Qualified-id routing for same-name Hosted deps across modules                   | ✅ (regression test `hosted_descriptor_routing_uses_qualified_id_across_modules`) |
| Async-runner support: async owner constructors awaited on parent                | ✅ (test `async_hosted_descriptor_collection_awaits_async_constructor`) |
| Hosted does NOT trigger single-thread fallback under capture                    | ✅      |
| Examples in `example/` and `example-tokio/` (TCP echo listener)                 | ✅ (`sharing/hosted_basic.rs` in both crates) |
| Sharing book chapter updated to document Hosted                                 | ✅      |
| `cargo build --all-features` green                                              | ✅      |
| `cargo clippy --no-deps --all-targets -- -Dwarnings` clean                      | ✅      |
| `cargo fmt --all -- --check` clean                                              | ✅      |
| `cargo test -p test-r-core --lib --all-features` green                          | ✅      |

Key landing surface (for Phase 2 / Phase 3 to build on):

- `test_r_core::internal::{HostedDep, DepScope::Hosted}` and
  `RegisteredDependency::hosted_codec`.
- `test_r_core::execution::{HostedOwner, HostedDescriptorCollection,
  DepWireBytes}` type aliases.
- `test_r_core::ipc::{IpcCommand::ProvideHostedDescriptor,
  IpcResponse::HostedDescriptorAccepted}`.
- `TestSuiteExecution::{has_hosted_dependencies,
  collect_hosted_descriptor_bytes_sync,
  collect_hosted_descriptor_bytes_async}`.
- Macro: `#[test_dep(scope = Hosted)]` (bare-ident or string form).
- Sync + tokio runners: parent collects Hosted descriptors once, stores
  owners in `_hosted_owners`, ships descriptor bytes to each worker via
  `provide_hosted_descriptor`.
- Examples: `example/src/sharing/hosted_basic.rs` and
  `example-tokio/src/sharing/hosted_basic.rs`.
- Unit tests: `test-r-core/src/execution.rs` (`cloneable_tests` module)
  covers descriptor collection, owner retention, qualified-id routing,
  async owner constructors, and macro/runtime backstop rejection.
- Docs: `book/src/advanced_features/dependency_sharing.md` updated to
  document the Hosted scope fully and remove the "coming soon" stub.

---

## Phase 2 — wasm-rquickjs migration (after Phase 1A)

The repo has 30+ `CompiledTest` deps and one `Arc<FullPreparedComponent>` dep.
All of them have the "compile once, share read-only" shape. None of them need
`Hosted`. This phase therefore unblocks immediately after 1A ships.

### 2.1 — Migrate `CompiledTest` to `Cloneable` (path-shipping)

- Add `serde::{Serialize, Deserialize}` to `CompiledTest`.
- Impl `CloneableDep` with `type Wire = CompiledTestPath` (a string).
- Parent owns the `NamedUtf8TempFile` and must keep it alive for the suite's
  duration. Until 1B, "the parent" can hold these directly; once 1B lands,
  these can optionally move into the dep host if we want them to be
  shutdown-coordinated.
- Apply to all 31 `compiled_*` deps with a one-line attribute change per file.

### 2.2 — Migrate `Arc<FullPreparedComponent>` to `Cloneable` (precompiled bytes)

- Parent: build the `CompiledTest`, then `Engine::precompile_component(&bytes)`
  once. Wire is `Vec<u8>` (precompiled native bytes).
- Introduce a `PerWorker` `WorkerEngine` dep holding a per-worker
  `wasmtime::Engine` configured identically to the parent's. Document the
  shared-config requirement and provide a helper.
- Worker reconstructor: `Component::deserialize(&engine.0, wire)` plus the
  background epoch ticker (spawned per worker).

### 2.3 — Address the shared `rt-target/` race

With parent-only compilation only the parent runs cargo; the previous
`use_shared_target: true` workaround for races simply becomes "the parent's
target dir". Update the docs of the test harness to reflect this.

### 2.4 — Validation

- Time `cargo test -p tests` before vs after.
- Run with `--test-threads $(nproc)` capture on, assert green.
- No test logic changes.

### Phase 2 exit criteria

- All `CompiledTest` and `FullPreparedComponent` deps annotated.
- Suite passes with parallel execution under capture.
- Wall-clock improvement documented in the test-r CHANGELOG and the
  wasm-rquickjs README.

---

## Phase 3 — golem migration (after Phase 1B)

The most complex phase: ~70 deps across 14 test binaries. Sub-phases are
shippable independently; ordering by smallest blast radius first.

### 3.1 — Quick wins: trivially `PerWorker` deps

(Requires only 1A; can start in parallel with Phase 2.)

Annotate as `PerWorker`:

- All 8 `Tracing` deps.
- All in-memory / SQLite / filesystem `Arc<dyn Get*Storage>` factories.
- All connection-pool deps (`SqlitePool`, `RedisPool`, `SqliteDb`, registry
  `Deps`).
- All session-store wrappers.

These compile with no other code changes and immediately restore parallelism
for the suites that only contain such deps (oidc sqlite/redis matrix tests,
registry-service sqlite tests, cli tracing).

### 3.2 — `Cloneable` for pure-data deps (1A)

- `IndexedStorageNamespaces` (`ns`, `ns2`) — add serde derives.
- `BlobStorageNamespace` variants — add serde derives.
- `Arc<dyn TestContext>` (4 deps across `agent_config` modules) — replace
  `dyn` with a tag enum; reconstruct on the worker side.

### 3.3 — `LastUniqueId` refactor (1A)

Seed each worker's counter with `worker_idx << 8` (test-r exposes
`worker_index()` as a helper) and annotate `PerWorker`. Falls back to a
`Hosted` "next id" RPC in 1B only if the 8-bit range proves too narrow.

### 3.4 — Component / cache deps via `Cloneable` (1A or 1B depending on path)

- The 14 `PrecompiledComponent` deps (generated by `test_component!`) change
  in one place — the macro definition in `golem-worker-executor-test-utils`.
- Parent runs AOT compilation once, ships native bytes via `Wire<Vec<u8>>`.
- Each worker has a `PerWorker` `WorkerEngine`-like dep that imports bytes
  directly. This requires `WorkerExecutorTestDependencies` to be reshaped: the
  on-disk component cache must be addressable without an `Arc` shared in
  process memory.

Two paths:
- **Path A (1A-only):** keep `WorkerExecutorTestDependencies` `Shared` for the
  binaries that use it (single-threaded fallback for those). Annotate
  everything else. Acceptable interim.
- **Path B (1B):** split `WorkerExecutorTestDependencies` into a `Hosted`
  owner (Redis + ports) and a `PerWorker` worker component (per-worker
  `component_writer` reading from a shared on-disk cache directory whose
  location is in the host descriptor).

### 3.5 — `Hosted` for `EnvBasedTestDependencies` and Docker containers (1B)

- `EnvBasedTestDependencies` — owner constructor stays; new
  `EnvBasedTestDependenciesClient::from_descriptor(desc)` re-opens gRPC
  clients from descriptor addresses.
- Docker container deps — descriptor is `{host, port}`. Some have richer
  surfaces (DB credentials), still small enough.
- `HttpTestContext` / `McpTestContext` become `PerWorker` deps consuming the
  Hosted cluster client.

### 3.6 — Suite-by-suite verification

PRs in roughly this order (smallest blast radius first):

1. `cli/golem-cli` — Tracing + `GeneratedPackage` (Cloneable path).
2. `golem-shard-manager`.
3. `golem-worker-service/tests/oidc`.
4. `golem-registry-service`.
5. `golem-service-base/blob_storage`.
6. `golem-worker-executor` matrix tests (key_value, indexed, rdbms, etc.).
7. `golem-worker-executor` main harness (after 3.4 path is chosen).
8. `golem-debugging-service`.
9. `integration-tests` (requires 1B).

Each PR keeps CI green.

### Phase 3 exit criteria

- Every `#[test_dep]` in the golem repo uses an explicit scope.
- Integration test suite runs with `--test-threads ≥ 2` under capture-on.
- Wall-clock improvement measured on the slowest CI lane and documented.

---

## Phase 1C — `HostedRpc`: built-in worker→owner RPC over the IPC socket

### Why

Today `Hosted` gives you "the owner is alive in the parent and here is a
descriptor"; the worker has to wire up its own transport (TCP listener,
gRPC client, Docker container handle, etc.) to actually *call* the owner.
That is the right model when the owner already speaks a real wire protocol
(`EnvBasedTestDependencies` is a real gRPC server; precompiled wasm
components don't need RPC at all). It is wasteful when the only thing the
test wants to do is "ask the singleton for the next id" or "tell the
singleton to reset" — every such case ends up re-implementing the same
TCP-loop with a custom protocol.

The concrete pain point in the existing migration plan: `LastUniqueId`
(Phase 3.3) is shipped today as `PerWorker` with a `worker_idx << 8`
seed and an explicit "fall back to a Hosted 'next id' RPC in 1B only if
the 8-bit range proves too narrow" caveat. With `HostedRpc` that
fallback becomes trivial.

### Design (sketch)

A new trait, derived by a new macro, that layers on top of `HostedDep`:

```rust
// User declares the service as a trait. Methods may take `&self` or
// `&mut self`; arguments and return values must implement
// `serde::{Serialize, DeserializeOwned}`.
#[test_r::hosted_rpc]
pub trait UniqueIdService {
    fn next(&self) -> u64;
    fn reset(&mut self);
}

// The owner is a plain Rust value that lives in the parent process. The
// hosted_rpc macro generates an `OwnerSide` dispatcher for it from the
// trait impl. No descriptor() / from_descriptor() boilerplate: the macro
// derives `HostedDep` and reserves a per-dep request/response channel
// over the existing IPC socket.
pub struct UniqueIdOwner { counter: AtomicU64 }

impl UniqueIdService for UniqueIdOwner {
    fn next(&self) -> u64 { self.counter.fetch_add(1, Ordering::SeqCst) }
    fn reset(&mut self)   { self.counter.store(0, Ordering::SeqCst); }
}

#[test_dep(scope = Hosted, rpc = UniqueIdService)]
fn unique_ids() -> UniqueIdOwner { UniqueIdOwner { counter: AtomicU64::new(0) } }
```

Tests then ask for the trait object — they never see `UniqueIdOwner`
itself:

```rust
#[test]
fn each_test_sees_unique_ids(ids: &dyn UniqueIdService) {
    let a = ids.next();
    let b = ids.next();
    assert_ne!(a, b);
}
```

What the macro generates behind the scenes:

```diagram
╭─────────────────────────────╮              ╭─────────────────────────────╮
│  Top-level parent           │              │  IPC worker subprocess      │
│                             │              │                             │
│  UniqueIdOwner (real value) │              │  UniqueIdServiceStub        │
│  + auto-derived             │  IPC socket  │  (auto-derived `impl`)      │
│  RpcDispatcher              │◀── RpcCall ──┤      ↑                      │
│       ↑                     │── RpcReply ─▶│      │                      │
│       │                     │              │  test calls `ids.next()`    │
╰───────┴─────────────────────╯              ╰─────────────────────────────╯
```

- New IPC frames: `RpcCall { dep_id, method_idx, args_bytes }` and
  `RpcReply { request_id, result_bytes | error }`. They piggyback on the
  same `interprocess` socket already used by `RunTest` / `ProvideHostedDescriptor`
  with a small request-id multiplexer so user RPC traffic doesn't starve
  test scheduling.
- Stub side: `UniqueIdServiceStub` holds the worker's IPC connection
  handle (or a clone of it behind a mutex) and a stable `dep_id`. Each
  method call serialises the args, writes one `RpcCall` frame, and
  blocks on the matching `RpcReply` frame.
- Dispatcher side: the parent runs a small async/sync dispatch loop per
  Hosted dep that pulls `RpcCall` frames off the socket and routes them
  to a generated `match method_idx { … }` over the user's owner value.
- `&mut self` methods serialise across all workers (the dispatcher holds
  an exclusive lock); `&self` methods can run concurrently. Tests that
  want stronger guarantees can mark methods `#[hosted_rpc(serialize)]`
  to force the lock.
- Errors (deserialize failure, owner panic, owner dropped) become a
  typed `HostedRpcError` returned by the stub method.

### Phase HR1.0 — IPC framing

- Add `IpcCommand::HostedRpcCall { request_id, dep_id, method_idx, args_bytes }`
  and `IpcResponse::HostedRpcReply { request_id, body }` where `body` is
  `Result<Vec<u8>, HostedRpcError>`.
- The worker's `Stream` gains a small request-id allocator + waiter
  table so multiple in-flight RPCs from concurrent async tests don't
  collide. (`test_threads()` is 1 inside an IPC worker, so the table
  only needs to handle re-entrant `tokio::spawn` cases.)
- Add a unit test that round-trips a large RPC payload (> 64 KiB) and
  a unit test that demonstrates multiplexing two concurrent in-flight
  calls.

### Phase HR1.1 — `HostedRpcDep` trait + `#[hosted_rpc]` macro

- Define `pub trait HostedRpcService` (marker, supertype of the user's
  trait). Define `HostedRpcDep` as a small private trait that registers
  the dispatcher table.
- Implement `#[hosted_rpc]` attribute macro on a user trait:
  - emits a `<Trait>Stub` struct that holds the IPC handle + dep id and
    implements `Trait` for it
  - emits an inherent `dispatch(&mut self, method_idx, args) -> bytes`
    on any owner that `impl`s the trait, generated from a method table
    keyed by stable index
  - generates `HostedDep::descriptor` / `from_descriptor` automatically
    (the descriptor carries only the dep id; the actual transport is
    the inherited IPC socket)
- Extend `#[test_dep(scope = Hosted, rpc = Trait)]` to wire the test
  parameter type as `&dyn Trait` (or `Arc<dyn Trait>`) and instantiate
  the stub on the worker side.

### Phase HR1.2 — Runner glue

- Parent's `_hosted_owners` Vec gains a parallel
  `Vec<HostedRpcDispatcher>`; one dispatcher loop per Hosted-RPC dep.
- The existing `test_thread` worker loop in `sync.rs` / `tokio.rs`
  recognises incoming `HostedRpcCall` frames at the parent end and
  routes them to the right dispatcher.
- Workers send `HostedRpcCall` from inside the stub; the worker thread
  pool's reader half routes the matching `HostedRpcReply` back to the
  blocked stub call.
- `RUST_BACKTRACE`-style trace propagation: owner panics surface to the
  caller as `HostedRpcError::OwnerPanicked { stack }` rather than
  killing the dispatcher.

### Phase HR1.3 — Examples, tests, docs

- New sync example: `example/src/sharing/hosted_rpc_basic.rs` —
  `UniqueIdService { fn next(&self) -> u64; }` exactly as above.
- New tokio example: `example-tokio/src/sharing/hosted_rpc_basic.rs` —
  async methods on the trait, exercised by `#[test] async fn …`.
- New book chapter section "Built-in RPC with `HostedRpc`" under
  `advanced_features/dependency_sharing.md`.
- Mode-consistency regression tests on par with Phase 1B: stub usage
  must work under capture-on, `--nocapture`, and `--test-threads N`.

### Phase HR1 exit criteria

- `#[hosted_rpc]` trait macro generates owner dispatcher + worker stub.
- `#[test_dep(scope = Hosted, rpc = Trait)]` wires the dep so tests
  receive `&dyn Trait` or `Arc<dyn Trait>`.
- IPC multiplexer round-trips concurrent in-flight RPCs without
  deadlock and without starving test scheduling.
- Owner panic surfaces as typed `HostedRpcError`, not a dead dispatcher.
- Sync + tokio examples, unit tests, integration tests, and book chapter
  in place.
- `cargo build / clippy / fmt / test` green.

### Phase 1C — shipped (MVP) — DONE

The MVP shipped in line with the oracle's pre-implementation scoping
advice: sync runner only, manual owner + manual stub, serialized
synchronous calls, no worker-side multiplexer / waiter table, tokio
deliberately deferred.

**Public surface (new):**

- `test_r::core::HostedRpcDep` (trait) with `type Stub`, `dispatch`,
  `build_stub`.
- `test_r::core::HostedRpcChannel`, `HostedRpcTransport`,
  `HostedRpcError { Dispatch(String), Transport(String) }`.
- `test_r::core::HostedRpcOwnerCell` — panic-safe parent-side cell.
- `test_r::core::InProcessHostedRpcTransport` for the `--nocapture`
  path.
- `test_r::core::RpcFactory { owner_into_cell, build_stub }` so the
  macro can give the runtime two type-erased fns per dep.
- `DepScope::HostedRpc` variant.
- `#[test_dep(scope = HostedRpc, stub = StubType)]` macro accepts the
  new `stub = ...` attribute and registers the dep under the stub
  type's path so tests can parameterise on `&Stub`.

**IPC additions:**

- `IpcResponse::HostedRpcCall { request_id, dep_id, method_idx, args_bytes }`
  (worker → parent).
- `IpcCommand::HostedRpcReply { request_id, body: HostedRpcReplyBody }`
  with `HostedRpcReplyBody::Ok { result_bytes } | Err { message }`
  (parent → worker).
- All three parent-side response loops (`Worker::run_test`,
  `provide_cloneable`, `provide_hosted_descriptor`) now dispatch
  incoming `HostedRpcCall` frames to a per-suite owner-cell map.
- The worker subprocess main IPC loop panics on an unexpected
  `HostedRpcReply` so out-of-protocol frames surface loudly.
- The tokio runner explicitly rejects `HostedRpc` traffic with a clear
  panic on every relevant arm (`HostedRpcReply` in the worker IPC
  loop; `HostedRpcCall` in `run_test` / `provide_cloneable` /
  `provide_hosted_descriptor`).

**Execution wiring:**

- `TestSuiteExecution::has_hosted_rpc_dependencies()` and
  `collect_hosted_rpc_owner_cells_sync()` mirror the Hosted equivalents
  and feed the parent's owner-cell map.
- Top-level parent: materialises owner cells once, builds an
  `Arc<HashMap<String, Arc<HostedRpcOwnerCell>>>`, passes it through
  `test_thread` and `Worker::set_hosted_rpc_owner_cells`.
- Worker subprocess: builds one stub per registered HostedRpc dep using
  `IpcHostedRpcTransport` on an `Arc<Mutex<Stream>>` shared with the
  main IPC loop, installs the stub via `provide_cloneable_value`.
- `--nocapture` / single-process mode: parent installs in-process stubs
  via `InProcessHostedRpcTransport` so tests see the same `Stub` value
  regardless of execution mode.

**Examples / tests / docs:**

- `example/src/sharing/hosted_rpc_basic.rs` — full worked
  `LastUniqueIdOwner` + `LastUniqueIdStub` example with **six tests**
  (positive ids, monotonic within a test, batch uniqueness, per-worker
  cross-suite call, owner-runs-only-in-top-level-parent regression,
  and the post-review **end-to-end IPC dispatch-error regression**
  `hosted_rpc_unknown_method_surfaces_as_dispatch_error` which routes
  an unknown method index through the real IPC pipeline and asserts
  the worker stub surfaces `HostedRpcError::Dispatch` AND that the
  stub remains usable for ordinary RPC calls afterwards — the error
  path must not desync the IPC framing).
- `test-r-core/src/execution.rs` cloneable_tests module gains four
  HostedRpc unit tests covering owner-cell collection, in-process
  transport round-trip, dispatch error surfacing, and transport-error
  surfacing.
- `book/src/advanced_features/dependency_sharing.md` gains a new
  `HostedRpc` section with the worked example, the "How HostedRpc
  works" walk-through, restrictions, a "when to prefer `Hosted`"
  note, and the post-review **MVP temporal invariant** subsection
  spelling out exactly when stub calls are safe (only from inside a
  running test body; never from `build_stub`; never from detached
  background work that outlives the test).
- `HostedRpcDep::build_stub` and `HostedRpcChannel::call` rustdoc in
  `test-r-core/src/internal.rs` now codify the same temporal
  invariant at the API surface so users see it at call sites.

**Validation (all green, re-run after post-review polish):**

- `cargo fmt --all`
- `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`
- `cargo check --all-features`
- `cargo test -p test-r-core --lib --all-features hosted_rpc -- --test-threads 1`
  (4 passed, 0 failed).
- `cargo test --all-features --lib sharing` (16 passed, 0 failed, was
  15 pre-review) — exercises both the parent IPC dispatch path and
  the in-process transport for all three Phase 1A/1B/1C scopes plus
  the new IPC error-path regression.
- `cargo test --all-features --lib sharing::hosted_rpc_basic -- --test-threads 4`
  (6 passed, 0 failed) — same six tests exercised against multiple
  worker subprocesses to confirm the IPC error-path test runs in the
  real subprocess-with-capture mode.
- `mdbook build book` passes.

**Oracle review outcome:**

Oracle reviewed Phase 1C twice (initial review + post-polish
follow-up) and confirmed the implementation is **MVP-correct and
Phase 1C is closable**. Three follow-up requests were filed in the
initial review and all are now addressed in the shipped code:

1. **Codify the MVP temporal invariant.** Done in
   `HostedRpcDep::build_stub` / `HostedRpcChannel::call` rustdoc and
   in `book/src/advanced_features/dependency_sharing.md`. Both spell
   out that stubs are only callable from inside a running test body
   and that `build_stub` must be cheap, side-effect-free, and must
   never call back into the channel.
2. **Keep defensive parent-side `HostedRpcCall` handling in every
   `provide_*` loop.** Already in `test-r-core/src/sync.rs` — the
   `provide_cloneable` and `provide_hosted_descriptor` parent loops
   both dispatch incoming `HostedRpcCall` frames to the owner-cell
   map identically to the `Worker::run_test` loop.
3. **Add an end-to-end IPC error-path test.** Done as
   `hosted_rpc_unknown_method_surfaces_as_dispatch_error` in
   `example/src/sharing/hosted_rpc_basic.rs`. The test runs in the
   real subprocess + capture mode, calls a deliberately unknown
   method index, and asserts the failure arrives as
   `HostedRpcError::Dispatch` rather than `Transport` AND that the
   stub keeps working after surfacing the dispatch error.

The post-polish follow-up review surfaced two **non-blocking
optional** items, both of which we also applied:

- **Drop / destructor wording** added to both the rustdoc on
  `HostedRpcChannel::call` and to the "MVP temporal invariant"
  subsection of the book — stubs must not be invoked from `Drop` /
  destructor-style cleanup or teardown hooks that may fire after the
  test body returns.
- **Owner-panic + poison unit test**
  (`hosted_rpc_owner_panic_surfaces_then_poisons`) added in
  `test-r-core/src/execution.rs`. **This test caught a real bug.**
  The original `HostedRpcOwnerCell::dispatch` acquired the lock
  *outside* the `catch_unwind` closure, which meant the `MutexGuard`
  was caught and dropped after the panic was already absorbed —
  leaving the mutex healthy. Subsequent calls re-entered the
  panicking owner instead of short-circuiting with the documented
  `"hosted rpc owner poisoned"` error. The fix moves the lock
  acquire *inside* the `catch_unwind` closure so the guard drops
  during unwinding, which is what poisons the mutex. Both the rustdoc
  on `HostedRpcOwnerCell::dispatch` and the inline comment now
  explain exactly why the lock has to live inside the closure.

**Deliberately out of MVP scope (oracle-confirmed, deferred):**

- `#[hosted_rpc]` trait macro (`scope = Hosted, rpc = Trait`) and
  auto-generated method-index dispatchers / stubs. (See HR1.1 below
  for the deferred plan.)
- tokio runner support — currently panics with a clear message on
  every relevant arm. (See HR1.2 below.)
- Concurrent / pipelined in-flight RPCs per worker (multiplexer +
  waiter table). (See HR1.0 below.)
- Reader thread on the worker subprocess (the MVP locks the IPC mutex
  for the full request/response, which is fine **only because** the
  worker's main IPC loop only reads frames between tests and stubs
  must obey the MVP temporal invariant above. Detached/background
  stub calls would desync the framing, which is why the temporal
  invariant is enforced by documentation rather than by code in the
  MVP.)

**Note on the HR1/HR2/HR3 sections below:** Those sections were
written before Phase 1C as the original aspirational scope for
HostedRpc (full trait macro, multiplexer, tokio, per-repo migration
plans). They remain in this document as the **future / deferred
plan** for `HostedRpc`, not as a description of what shipped. The
shipped MVP is the smaller surface above. Anything in HR1/HR2/HR3
that contradicts the shipped MVP is to be read as "what we want to
build later", not "what is in the code today".

---

## Phase HR2 — wasm-rquickjs migration audit (after HR1)

**Expected outcome: no migration needed.**

Re-audit the 31 `CompiledTest` and 1 `Arc<FullPreparedComponent>` deps
against `HostedRpc`. Both shapes are "compile once, ship native bytes",
not "call a method on a singleton" — so `Cloneable` (Phase 2) remains
the right tool and `HostedRpc` does not help here.

The only optional candidate is the per-suite `NamedUtf8TempFile` lifetime
that Phase 2.1 currently parks on the parent: if shutdown coordination
later proves awkward, a thin `HostedRpc` `TempFileRegistry` with
`alloc(name) -> PathBuf` / `release(name)` would replace the implicit
shared parent state with an explicit owner. Track as a "do only if
needed" follow-up, not an active migration item.

---

## Phase HR3 — golem migration (after HR1)

Three concrete migration targets, each one a worked example of the
HostedRpc shape.

### HR3.1 — `LastUniqueId` (THE motivating example)

**Today (Phase 3.3 of the existing plan):**

```rust
// PerWorker dep that seeds itself with `worker_idx << 8`. Each worker
// owns its own 8-bit slice of the id space. Fragile if a suite ever
// allocates more than 256 ids per worker.
#[test_dep(scope = PerWorker)]
fn last_unique_id(WorkerIndex(idx): WorkerIndex) -> LastUniqueId {
    LastUniqueId::seeded(((idx as u64) << 8) | 1)
}
```

**With HostedRpc:**

```rust
#[test_r::hosted_rpc]
pub trait UniqueIds {
    fn next(&self) -> u64;
}

pub struct LastUniqueIdOwner(AtomicU64);
impl UniqueIds for LastUniqueIdOwner {
    fn next(&self) -> u64 { self.0.fetch_add(1, Ordering::SeqCst) }
}

#[test_dep(scope = Hosted, rpc = UniqueIds)]
fn unique_ids() -> LastUniqueIdOwner {
    LastUniqueIdOwner(AtomicU64::new(1))
}

#[test]
fn allocates_globally_unique_ids(ids: &dyn UniqueIds, /* … */) {
    let a = ids.next();
    let b = ids.next();
    assert_ne!(a, b);
}
```

Effect: a single global monotonically increasing counter shared by
every worker, no `<< 8` seed games, no risk of overflow, no test rewrites
beyond changing the type of the dep parameter from `&LastUniqueId` to
`&dyn UniqueIds`.

### HR3.2 — Docker container management deps (subset of Phase 3.5)

**Today (planned):** descriptor carries `{host, port}`, each worker
opens its own Docker SDK client to talk to the daemon — even though
many tests only ever want "tear down this container" or "is it healthy?"

**With HostedRpc:** the container handle stays in the parent (one Docker
SDK client per suite); workers call methods on a trait:

```rust
#[test_r::hosted_rpc]
pub trait RedisContainer {
    fn connection_string(&self) -> String;          // pure data, fine
    fn flush_all(&self);                            // imperative side-effect
    fn snapshot_metrics(&self) -> RedisMetrics;     // serialised result
}
```

`connection_string()` is the only thing the test needs at warm-up time
(workers then connect to Redis with the returned URL just like today).
`flush_all()` / `snapshot_metrics()` are the new things that today
require per-worker re-implementation; here they are one method each.

Note: bulk-data deps that already speak gRPC (`EnvBasedTestDependencies`
itself) keep the **plain `Hosted` + descriptor + gRPC client** shape from
Phase 3.5. HostedRpc is for the "control-plane glue" around them, not
for replacing gRPC.

### HR3.3 — `WorkerExecutorTestDependencies` split (refines Phase 3.4 Path B)

The Phase 3.4 Path B plan already splits this dep into a Hosted owner
(Redis + ports) and a `PerWorker` component cache. With HostedRpc the
Hosted owner gets a small explicit trait surface:

```rust
#[test_r::hosted_rpc]
pub trait TestEnv {
    fn redis_url(&self) -> String;
    fn allocate_port(&self) -> u16;          // monotonic, never reused
    fn registered_components(&self) -> Vec<ComponentRef>;
}
```

That removes the awkward "parent-held but workers reach in via globals"
shape from the Phase 3.4 sketch and makes the Hosted/PerWorker split
self-documenting.

### Phase HR3 exit criteria

- `LastUniqueId` (HR3.1) lands as the first HostedRpc adopter in golem.
- At least one Docker-container dep in `EnvBasedTestDependencies`
  exposes a HostedRpc control surface alongside its existing gRPC
  client (HR3.2).
- `WorkerExecutorTestDependencies` is refactored per HR3.3 OR documented
  as "stays on plain `Hosted` because the control surface is empty".
- No test logic changes beyond dep parameter types.
- `cargo test` green in CI on the affected golem packages.

---

## HostedRpc — risks and open questions

| Risk / question                                                              | Mitigation / current thinking                                                                                                                                          |
|------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| RPC traffic starves test scheduling on the shared IPC socket.                | Request-id multiplexer + bounded queue; per-dep dispatcher loop; benchmark with `UniqueIds` under contention before declaring HR1 done.                                |
| `&mut self` methods serialised across all workers — surprise contention.     | Document as the trade-off; provide `#[hosted_rpc(parallel)]` opt-out for `&self` methods if our default ever changes.                                                  |
| Owner panic kills the dispatcher.                                            | Wrap each dispatch in `catch_unwind`; return `HostedRpcError::OwnerPanicked` to the calling stub; mark the dep dead so subsequent calls fail fast instead of hanging.  |
| Worker drops mid-call → orphan in-flight RPC at the parent.                  | Parent watches worker connection EOF; on drop, fail every pending stub future for that worker with `HostedRpcError::WorkerDisconnected`.                               |
| Serialization framework lock-in (serde-cbor? bincode? desert_rust?).         | Reuse the existing `desert_rust` codec already used for IPC frames; document the constraint in the macro error message.                                                |
| Trait method ordinals change when methods are reordered → silent mismatch.   | Macro derives the method table from the trait at the call site; stub and dispatcher are compiled from the same trait crate, so they always agree.                      |
| Trait with associated types or generics.                                     | Out of scope for HR1. Reject in the macro with a clear error.                                                                                                          |
| Async methods on the trait.                                                  | Supported in HR1.3 via the tokio runner; stub awaits an `oneshot` channel keyed by `request_id`. Sync runner rejects async methods at macro time.                      |
| What about Hosted deps that need BOTH a descriptor AND RPC (e.g. Redis URL + control)? | `scope = Hosted, rpc = Trait` already covers this — the auto-generated descriptor carries the dep id and the user puts the URL on the trait as `fn url(&self)`.       |

---

## Cross-cutting risks and mitigations

| Risk                                                                    | Mitigation                                                              |
|-------------------------------------------------------------------------|-------------------------------------------------------------------------|
| IPC framing too narrow for Cloneable payloads.                          | **Fix first in 1A.0** (`u32` length prefix, shared helper).             |
| Public `RegisteredDependency` change breaks downstream.                 | Treat as breaking; bump version; add `new_shared` constructor.          |
| `Cloneable`/`Hosted` owner panics in parent / dep host.                 | Wrap in `catch_unwind`; fail the whole suite with a clear error.        |
| Dep host crashes mid-suite.                                             | Parent monitors via `wait`; on death, SIGKILL workers, exit non-zero.   |
| Wasmtime precompile/deserialize tied to engine config.                  | Provide a shared `Engine` config helper in `test-r` examples + check.   |
| `Wire<T>` / `Descriptor<T>` param parsing breaks current DI assumptions.| Treat them as macro-recognised special types, not `DependencyView`.     |
| Cross-locus dep graph mistakes (PerWorker dep used by parent owner).    | Validate at macro time using recorded `owner_deps`/`worker_deps` sets.  |
| Dep host stdout mixed into per-test capture.                            | Route host output separately, prefixed with `[dep-host]`.               |

## Open questions to settle in 1A.1

1. Auto-derive helper for the common case where `Wire = Self` and `T: Serde`?
   (Deferrable; pure-data Cloneable users can write the impl by hand for now.)
2. How does `worker_index()` get into worker constructors? Special parameter
   type `WorkerIndex(usize)`, or a thread-local?
3. Should `Shared` deps in a Hosted suite live in the dep host or stay in the
   parent? (Default: dep host — simpler lifetime story.)
4. Cancellation propagation: if a worker crashes mid-test, do other workers
   continue? (Today: yes. Plan: keep that semantics; dep host is unaffected.)
