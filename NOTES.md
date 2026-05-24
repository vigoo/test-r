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
> - **Phase HR1.0 (IPC framing regression tests) — DONE.** The
>   `IpcResponse::HostedRpcCall` / `IpcCommand::HostedRpcReply` frames
>   already carry the spec'd `request_id` field (added in 1C), and the
>   IPC framing uses a `u32` length prefix that supports payloads up to
>   4 GiB. HR1.0 closes the test gap with three new regressions: an
>   in-process transport test that round-trips a >64 KiB reply (256 KiB)
>   verifying byte-exact framing; an in-process transport test that
>   spawns 4 std::threads × 32 calls each (128 concurrent in-flight
>   calls over the shared transport) and asserts no deadlock / no
>   duplicate ids / monotonic sort; an end-to-end IPC large-payload
>   round-trip test in both `example/src/sharing/hosted_rpc_basic.rs`
>   and `example-tokio/src/sharing/hosted_rpc_basic.rs`; plus a tokio
>   `tokio::join!` two-concurrent-calls regression in the tokio
>   example. The MVP's serialise-via-mutex per-worker transport
>   correctly handles all of these; a true concurrent-multiplexer
>   reader-task design is intentionally deferred (the MVP temporal
>   invariant — stub calls only from inside a running test body —
>   makes it unnecessary for the existing use cases). All eight
>   sharing tests pass under both `--test-threads 4` and
>   `--spawn-workers --test-threads 4` for both runners.
> - **Phase HR1.2 (tokio HostedRpc) — DONE.** The tokio runner now matches
>   the sync runner's HostedRpc MVP. Owner cells and `RpcFactory` lookup
>   are materialised in the top-level parent on the tokio runtime; every
>   tokio worker subprocess installs IPC-backed stubs (or in-process
>   stubs in the no-spawn-workers / `--nocapture` path) so async tests
>   see the same `Stub` value as their sync counterparts. The previously
>   placed "tokio rejects HostedRpc" panics in `Worker::run_test`,
>   `provide_cloneable`, and `provide_hosted_descriptor` have been
>   replaced with real `handle_hosted_rpc_call` dispatch. The new tokio
>   transport bridges `HostedRpcTransport::call` (sync trait) to the
>   tokio IPC primitives via `tokio::task::block_in_place` +
>   `Handle::current().block_on(...)`, sharing the existing
>   `Arc<Mutex<Stream>>` connection with the main IPC loop. A full
>   tokio mirror of the sync example sits in
>   `example-tokio/src/sharing/hosted_rpc_basic.rs` (8 tests after HR1.0:
>   positive ids, monotonicity, batch uniqueness, per-worker cross-suite
>   call, the parent-only-singleton invariant, the end-to-end IPC
>   dispatch-error regression, the HR1.0 large-payload >64 KiB round
>   trip, and the HR1.0 `tokio::join!` two-concurrent-calls regression).
>   All 8 tokio tests pass under both `--test-threads 1` and
>   `--spawn-workers --test-threads 4`. The MVP temporal invariant
>   documented in 1C still applies unchanged.
> - **Phase HR1.1 (`#[hosted_rpc]` trait macro) — DONE.** A new
>   `#[hosted_rpc]` attribute macro replaces the per-method
>   `HostedRpcDep` boilerplate in `test-r-macro/src/hosted_rpc.rs` and is
>   re-exported as `test_r::hosted_rpc`. Applied to a user trait, it
>   emits a `<Trait>Stub { channel: HostedRpcChannel }` worker-side
>   struct that implements the trait by routing each method through
>   `HostedRpcChannel::call(method_idx, ...)`, plus a `<Trait>Dispatch`
>   helper trait blanket-implemented for every `T: <Trait>` that
>   exposes `dispatch_<snake_case>(method_idx, args)` so the owner's
>   `HostedRpcDep::dispatch` becomes a one-line delegation. Args are
>   `desert_rust`-encoded as a tuple of the parameter types (1-arg
>   methods send the bare value rather than `(T,)` to dodge a
>   `desert_rust 0.1.7` asymmetry between `BinarySerializer for (T,)`
>   and `BinaryDeserializer for (T,)`; 0-arg methods send `()`); the
>   reply is encoded directly. `test-r-core` now re-exports
>   `desert_rust` as `#[doc(hidden)] pub use desert_rust;` so the macro
>   can reach it via `::test_r::core::desert_rust::...` without forcing
>   downstream crates to add a new dependency. Transport / codec /
>   dispatch panics in the generated stub now include the
>   `Trait::method` label in their `expect(...)` text. The MVP
>   restrictions are enforced at macro time with `compile_error!` and
>   cover: attribute arguments on `#[hosted_rpc(...)]` itself, generic
>   traits/methods, supertraits, `unsafe trait` / `unsafe fn`,
>   non-default ABI (`extern fn`), variadic methods, default-impl
>   methods, `async fn` methods, by-value `self` / explicit `self: T`,
>   `impl Trait` in argument or return position (walked recursively
>   without pulling in `syn`'s optional `visit` feature),
>   non-identifier argument patterns (`_`, destructuring),
>   `#[cfg(...)]` / `#[cfg_attr(...)]` on the trait or its methods (the
>   generated sibling items aren't cfg-propagated in the MVP), and
>   **`&mut self` receivers** (test-r injects test deps as `&Stub`
>   immutable references, so `&mut self` stub methods would compile
>   but be uncallable from a normal `#[test] fn (s: &MyStub)`
>   parameter — the macro rejects up-front to avoid that UX trap).
>   All 22 rejection rules and happy paths (incl. 2-arg methods and
>   unit-return methods) are covered by 22 unit tests in
>   `test-r-macro/src/hosted_rpc.rs`. End-to-end examples live in
>   `example/src/sharing/hosted_rpc_macro.rs` and
>   `example-tokio/src/sharing/hosted_rpc_macro.rs` (7 tests each,
>   covering 0-arg `next`, 1-arg `reserve`, non-primitive 1-arg
>   `echo(String)`, 2-arg `add(a, b) -> u64` (pins the multi-arg
>   `(T1, T2)` wire shape), unit-return `ping(&self)` (pins the
>   `ReturnType::Default` empty-reply framing), intra-test
>   monotonicity, and the parent-only-singleton invariant); all 7+7
>   pass under both `--test-threads N` and
>   `--spawn-workers --test-threads N` on both runners. The
>   dependency-sharing book chapter has a new
>   "`#[hosted_rpc]` attribute macro" sub-section listing the full
>   restriction set (incl. `&self`-only), the wire format and the
>   panic-on-infrastructure-failure semantics; the stale
>   "no `#[hosted_rpc]` codegen yet" note in the MVP-scope bullet has
>   been replaced with a forward link. Oracle-reviewed twice (HR1.1
>   initial + hardening pass) — second review specifically asked for
>   the `&mut self` rejection and the unit-return e2e test, both
>   landed here. `cargo fmt`, `cargo clippy --no-deps --all-targets
>   --all-features -- -Dwarnings`, `cargo test -p test-r-macro` (22
>   passed), `cargo test -p test-r-core --lib hosted_rpc` (7 passed),
>   the full sync sharing suite (29 tests), the full tokio sharing
>   suite (27 tests), and `mdbook build book` all green.
> - **Phase HR2 (wasm-rquickjs HostedRpc audit) — DONE: no migration needed.**
>   Re-audit of every `#[test_dep]` in `golemcloud/wasm-rquickjs` (audited
>   2026-05; `rg "scope = " tests/` + `rg "#\[test_dep" tests/`) confirms
>   that all deps fall into one of two existing categories: 40+ `compiled_*`
>   `Cloneable` deps that ship the canonicalised wasm path (Phase 2
>   migration), and one `PerWorker` `FullPreparedComponent` that builds
>   the wasmtime `Engine`/`Linker`/`Component` per worker from the
>   parent-shipped path. (Note: the HR2 plan section below mentions "31
>   `CompiledTest` deps" — that count predates later test growth; the
>   current re-audit finds 40+. The shape conclusion is unchanged.) None of them describe "a method on a singleton
>   service" — they are all "compile once, ship native bytes" shapes,
>   which HostedRpc explicitly does not help with. The optional
>   `TempFileRegistry` follow-up (mentioned in the HR2 plan section
>   below) is parked as "do only if needed" — Phase 2.1's parent-held
>   `NamedUtf8TempFile` shutdown coordination is working fine in
>   practice, so no proactive HostedRpc migration is warranted here.
>   This matches the HR2 plan's "Expected outcome: no migration needed"
>   line; the section is kept as a forward-looking record of the audit.
> - **Phase HR1.3 (examples, tests, docs — validation-only closure) — DONE.**
>   HR1.3's deliverables were progressively landed across HR1.0 (IPC
>   framing + first regressions), HR1.1 (`#[hosted_rpc]` macro + macro
>   unit tests + extra e2e tests + book sub-section), and HR1.2 (tokio
>   parity + tokio mirror example), so HR1.3 closes with **no new code
>   or new files** — only the remaining mode-consistency criterion is
>   exercised. Concretely, every hosted_rpc test in both
>   `example/src/sharing/hosted_rpc_{basic,macro}.rs` and
>   `example-tokio/src/sharing/hosted_rpc_{basic,macro}.rs` (15 tests
>   on each runner: 8 basic + 7 macro) passes under all four
>   harness invocations exercised below — the default capture-on /
>   single-thread invocation (`cargo test`), `--test-threads 4` (still
>   capture-on, no spawn), `--nocapture` (in-process transport, no
>   spawn), and `--spawn-workers --test-threads 4` (IPC-backed
>   transport, spawn) — on both runners. That covers both
>   `InProcessHostedRpcTransport` (no-spawn path) and the IPC-backed
>   transport (spawn path) without any per-test gating.
>   Two HR1 plan items remain deferred (and have been documented as
>   such throughout HR1.0/1.1/1.2): native `async fn` trait methods on
>   the stub (HR1.1 rejects them at macro time; the enclosing test fns
>   can still be `async` because the tokio runner already bridges them
>   via `block_in_place`), `&dyn Trait`/`Arc<dyn Trait>` test
>   parameters (the macro generates a concrete `<Trait>Stub` type
>   today; users parameterise tests on `&<Trait>Stub`), and the true
>   worker-side reader-task / waiter-table multiplexer for concurrent
>   in-flight RPCs on a single IPC connection (the MVP's serialise-
>   via-mutex per-worker transport correctly handles all current use
>   cases under the MVP temporal invariant). HR1.3 closure therefore
>   marks "examples, tests, docs" complete; the deferred bullet items
>   remain captured in this NOTES file for a later phase. Oracle
>   approved closing HR1.3 in the HR1.1 second-pass review summary.
>   Validation: `cargo test -p test-r-example --lib sharing::hosted_rpc`
>   (default capture-on, 15 passed), `... -- --test-threads 4`
>   (capture-on, 15 passed), `... -- --nocapture` (15 passed),
>   `... -- --spawn-workers --test-threads 4` (15 passed); same four
>   invocations on `test-r-example-tokio` (15 passed each).
> - **Phase 2 — DONE.** wasm-rquickjs has migrated all 40 `compiled_*`
>   `#[test_dep]`s to `scope = Cloneable` (path-shipping) and applied the
>   oracle's D3 split to `node_compat.rs`: a single `Cloneable`
>   `CompiledTest` test_dep (`compiled_node_compat_full`) replaces the old
>   parent-only compile path, and a `PerWorker` `FullPreparedComponent`
>   test_dep (`prepare_node_compat_full`) materialises the wasmtime
>   `Engine` / `Linker` / `Component` per worker subprocess from the
>   parent-shipped wasm path. `CloneableDep for CompiledTest` ships the
>   canonicalised wasm path (panicking loudly on `OwnedTemporary` from
>   `plug_into`, which only ever appears inside test bodies). Result:
>   capture-on no longer collapses the runtime / errors / node_compat
>   harnesses to single-threaded mode, and AGENTS.md's old "ALWAYS pass
>   `--nocapture`" note has been removed. See "Phase 2 — wasm-rquickjs
>   migration (after Phase 1A)" below for the executed plan.
> - Phase 3.1 — done: bare `PerWorker` annotations applied to the
>   trivially safe deps across 12 golem test files; workspace `test-r`
>   repointed to the local path. Capture+parallel is proven on the
>   `golem-worker-executor` lib unit-test binary
>   (`src/services/oplog/tests.rs`), the first golem binary whose
>   dependency graph is fully non-`Shared`.
>   `golem-service-base/tests/blob_storage.rs` is also now fully
>   non-`Shared` and was smoke-tested under capture+parallel to
>   confirm it no longer falls back to serial execution, while oplog
>   remains the proof that non-empty captured output is attributed
>   correctly. Most other touched binaries still contain at least
>   one `Shared` dep and continue to fall back to single-threaded
>   mode under capture until 3.2–3.5 land.
> - Phase 3.2 — done as `PerWorker` annotations (not `Cloneable`):
>   the targeted deps are trivially cheap to construct, the inner
>   production enums lack `serde` derives, and the `Arc<dyn TestContext>`
>   case would require a workspace-wide dimension/signature refactor.
>   See §3.2 status for the per-file list and reasoning. No new
>   fully-non-`Shared` binaries emerge from 3.2; the Phase 3.1
>   capture+parallel proofs still hold unchanged.
> - Phase 3.3 — done. test-r now exposes `worker_index()` (CLI flag
>   `--worker-index`, runner sets it on each spawned worker, helper
>   defaults to 0 in the parent / no-spawn-workers path); golem's
>   `LastUniqueId` factories in `golem-worker-executor` and
>   `golem-debugging-service` migrated to `PerWorker` seeded with
>   `(worker_index() << 8)`. Capture+parallel JUnit on the example
>   proves the index reaches the constructor in spawned workers.
>   See §3.3 status for details.
> - Phase 3.4 — DONE (Hosted fixture-cluster pattern; see §3.4 status).
> - Phase 3.5 — DONE (3.5.0 `AsyncHostedDep`; 3.5.1 first
>   `EnvBasedTestDependencies` Hosted consumer; see §3.5 status).
> - Phase 3.6 — DONE (explicit-scope sweep across the golem repo;
>   see §3.6 status).
> - Phase 3.6 deferred exit criteria (integration-suite
>   capture+parallel, wall-clock on slowest CI lane) — no longer
>   blocked on the HR3.3 implementation. The golem-side cluster-control
>   migration is done locally and proves that the single-thread
>   Shared-dependency fallback is gone. Strict full-suite green remains
>   pending because two unrelated integration tests failed in the local
>   two-thread spawned-worker run; wall-clock documentation still needs
>   the slowest CI lane.
> - **HR1 deferred items — closed as "won't ship unless triggered".**
>   Native `async fn` trait methods on the stub, `&dyn Trait` /
>   `Arc<dyn Trait>` test parameters, and the worker-side reader-task /
>   waiter-table multiplexer are all explicitly closed with rationale
>   in the "HR1 deferred items — closure" table below. The shipped
>   sync-stub + concrete-`<Trait>Stub` + serialise-via-mutex transport
>   covers every demonstrated adopter; the deferred items remain
>   captured as future work for if/when a real need appears.
> - **HR2 — closed as "won't ship unless triggered".** Phase 2.1's
>   parent-held `NamedUtf8TempFile` shutdown coordination has shown no
>   issues; the optional `TempFileRegistry` HostedRpc migration is
>   removed from the active backlog. See the HR2 section closure note.
> - **Open questions (§1A.1) — all four settled.** See the
>   "Open questions — settled" section at the bottom of this file.

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
| Compile-fail / trybuild tests for all macro misuse cases.                   | ✅ (covered by in-process macro-expansion tests in `test-r-macro/src/hosted_rpc.rs::tests` (22 rejection tests asserting `compile_error!` in the expanded output) and `test-r-macro/src/deps.rs::worker_view_tests` (14 rejection tests on the `worker = ...` parser). These exercise the macro itself rather than going through a separate `trybuild` `.stderr` snapshot layer — the coverage is functionally equivalent for the deferred misuse paths and avoids `.stderr` brittleness across rustc versions.) |

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

### Phase 2 — shipped — DONE

The wasm-rquickjs migration shipped in line with the oracle's
pre-implementation D3-split guidance:

**`tests/common/mod.rs`:** added a `CloneableDep for CompiledTest` impl
that ships the **canonical absolute** wasm path (`canonicalize_utf8()`)
over the wire. The reconstructor asserts the path exists on disk in
the worker. The `OwnedTemporary` variant (only ever produced by
`plug_into`, which is called inside test bodies, never inside a
`#[test_dep]` ctor) panics loudly in `to_wire` to prevent silent
deleted-file races.

**Runtime / errors harnesses (40 `compiled_*` deps total across
`tests/runtime/*.rs` + `tests/errors.rs`):** every
`#[test_dep(tagged_as = "…")]` was rewritten to
`#[test_dep(tagged_as = "…", scope = Cloneable)]`. Parent compiles
each wrapper crate once and ships the path; workers each rebuild a
`Precompiled(...)` `CompiledTest` from the path.

**`tests/node_compat.rs` (D3 split):** the single
`prepare_node_compat_full` `Shared` dep was split into two:

```rust
#[test_dep(tagged_as = "node_compat_full_compiled", scope = Cloneable)]
async fn compiled_node_compat_full() -> CompiledTest { … }

#[test_dep(scope = PerWorker)]
fn prepare_node_compat_full(
    #[tagged_as("node_compat_full_compiled")] compiled: &CompiledTest,
) -> Arc<FullPreparedComponent> { … }
```

This confirms an important Phase 1A property the docs were quiet
about: a `PerWorker` constructor **can** take a worker-visible
`Cloneable` dep as a parameter. (The Cloneable owner runs once in the
parent and the bytes get reconstructed into a worker-local value
before any `PerWorker` ctor that depends on it runs.) The
`FullPreparedComponent` itself stays per-worker (each worker owns its
own `wasmtime::Engine` + `Linker` + `Component` + epoch-ticker
thread) but the expensive cargo-build runs exactly once.

**Validation:**

- `cargo check --tests` (workspace) → green.
- `cargo clippy --tests -- -Dwarnings` → green.
- `cargo fmt` → no diff.
- `cargo test --test errors -- --test-threads 4` → **12 passed in
  53.7s under capture on, with visible interleaved test starts/finishes
  in the output proving parallel worker execution** (would have been
  serialised pre-Phase 2).
- `cargo test --test runtime path -- --test-threads 4` → 11 passed in
  45.9s under capture on.
- `cargo test --test node_compat parallel__test_buffer -- --test-threads 4` →
  **172 passed in 228.4s under capture on**, exercising the D3 split
  end-to-end: a single parent-side compile + 4 worker-local prepare
  passes + interleaved subprocess test runs.

**Capture correctness — JUnit / CTRF round-trip:**

"Tests are parallel and don't print" is not enough — it could mean
output is being silently dropped or commingled across worker
subprocesses. So Phase 2 was re-validated using test-r's structured
reporters that **embed** captured per-test stdout/stderr, with a
mechanical cross-contamination scan to prove every test's bytes end
up under that test (and no other):

- `cargo test --test errors -- --test-threads 4 --format junit
  --logfile … --show-output` produced a JUnit XML with 12
  `<testcase>` entries. Each `<system-out>` carried a **distinct**
  `[stderr] thread '<unnamed>' (1) panicked at src/internal.rs:LINE:COL`
  from the *that* test's wasm subprocess panic (different src lines,
  different panic messages per test). A scripted cross-contamination
  scan (search every test's captured bytes for *other* tests' names)
  reported **0 / 12** contaminated.
- `cargo test --test runtime buffer -- --test-threads 4 --format
  ctrf --logfile … --show-output` produced a CTRF JSON with 7
  passing tests. Cross-contamination scan reported **0 / 7**. Each
  test's own `println!("Output:\n…")` showed up only under its own
  `stdout` field.
- `cargo test --test node_compat parallel__test_buffer_concat_js
  -- --test-threads 4 --format ctrf --logfile … --show-output`
  produced CTRF JSON with the test passing and its full captured
  `[stderr] DeprecationWarning: Buffer() is deprecated…` chain
  attributed to that test (and only that test) — confirming the D3
  split (parent compile + per-worker prepare + IPC subprocess
  capture) preserves byte-accurate output routing.

Verdict: output capture is not merely "not printed to the terminal"
post-Phase 2 — it is correctly attributed per test in the structured
reporters, with zero cross-test leakage detected.

**Docs / repo state updates:**

- `wasm-rquickjs/Cargo.toml` workspace dep was repointed from
  `test-r = "3.0.3"` to a local path
  (`test-r = { path = "../../oss/test-r/test-r" }`) for the duration
  of the migration — flagged with a comment to revert once test-r
  3.0.5+ is published with the Phase 1A+ APIs.
- `wasm-rquickjs/AGENTS.md` had its old "ALWAYS pass `--nocapture`
  when running `cargo test --test node_compat` locally" note replaced
  with a Phase 2 note that says `--nocapture` is no longer needed for
  parallel execution.

**Out of Phase 2 scope (deferred):**

- The original Phase 2.2 plan called for precompiled-bytes shipping
  (parent does `Engine::precompile_component(&bytes)`, ships bytes,
  workers do `Component::deserialize(...)` against a `PerWorker`
  `WorkerEngine`). The D3 split avoids that work entirely because the
  parent-compiled wasm file already lives on disk; workers load it
  with their per-worker Engine. Precompiled-native-bytes shipping is
  a possible future optimisation only if disk loading becomes a
  meaningful share of suite wall-clock.
- The `rt-target` race section (2.3) is implicitly resolved by the
  D3 split — parent owns the only cargo invocation per dep — but the
  formal docs update for the wasm-rquickjs README is still a TODO if
  the README ever called the workaround out (it currently does not).

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

These compile with no other code changes and restore parallelism only for
suites whose full dependency graph becomes non-`Shared`; the 3.1 status
section below records which binaries that is actually true for after the
landed changes.

#### 3.1 — Status (DONE)

Workspace `test-r` dep repointed to local path:
`test-r = { path = "../../oss/test-r/test-r" }`.

`PerWorker` annotations applied across these files (no behavioural code
changes beyond the attribute):

- `cli/golem-cli/tests/lib.rs` — `tracing`.
- `golem-shard-manager/tests/lib.rs` — `tracing`.
- `golem-registry-service/tests/lib.rs` — `tracing`.
- `golem-registry-service/tests/repo/sqlite.rs` — `db_pool`, `deps`.
- `golem-worker-executor/src/services/oplog/tests.rs` — `tracing`
  (in-`src/` unit-test module).
- `golem-worker-executor/tests/lib.rs` — `tracing`.
- `golem-debugging-service/tests/lib.rs` — `tracing`.
- `golem-worker-service/tests/oidc/lib.rs` — `tracing`,
  `sqlite_store_file`, `sqlite_pool`, `sqlite_store_default`,
  `sqlite_store_fast_expiry`. (Redis chain deliberately left `Shared`
  until 3.5; it spawns Redis on a fixed port.)
- `golem-service-base/tests/blob_storage.rs` — all 8 deps
  (`in_memory`, `fs`, `s3`, `s3_prefixed`, `sqlite`, `cc`, `co`, `cs`).
  The MinIO container is launched lazily inside `get_blob_storage()`,
  not by the dep constructor, so the dep itself is safely per-worker.
- `integration-tests/tests/lib.rs` — `tracing`.
- `integration-tests/tests/agent_config_live_mutation.rs` — `tracing`.

Deferred intentionally to later sub-phases:

- `bridge_gen/{rust,typescript}.rs` `GeneratedPackage` deps — each runs
  `cargo check` + `cargo build`; bare `PerWorker` would re-run that work
  in every worker. Better fit for 3.2 (`Cloneable` with a path wire-form
  plus a held TempDir).
- `LastUniqueId` — needs a runtime helper (`worker_index()` / equivalent)
  to seed counters; that's 3.3.
- `WorkerExecutorTestDependencies`, `EnvBasedTestDependencies`,
  Docker container deps, Redis spawners — these are 3.4 / 3.5 (Hosted).
- `oidc/handler.rs` `oidc_handler` — depends on the still-`Shared`
  `default_session_store` (redis chain), so its scope is left default
  until that chain is migrated in 3.5.

##### Compile validation

`cargo check --tests` is green for: `golem-cli`, `golem-registry-service`,
`golem-debugging-service`, `golem-worker-service`, `golem-service-base`,
`golem-worker-executor`, `golem-shard-manager`, `integration-tests`.
`cargo fmt --all -- --check` is clean.

The build required dropping unused-stray `wasi:clocks@0.3.0-rc-...`
`*.wit` files left over in the worktree (a half-finished WIT bump
unrelated to test-r) and re-running `cargo make wit` to re-materialise
the wasi p2 dep tree.

##### Capture+parallel validation

Capture+parallel is currently proven for exactly one golem test
binary: `golem-worker-executor`'s in-`src/` unit-test module
(`src/services/oplog/tests.rs`). It is the first golem binary whose
entire dep graph is non-`Shared` after Phase 3.1 (its only dep,
`tracing`, is now `PerWorker`).

`golem-service-base/tests/blob_storage.rs` is the second golem
binary whose entire dep graph is now non-`Shared` after Phase 3.1
(all 8 deps are `PerWorker`). Smoke-tested under capture-on,
`--test-threads=4`, JUnit logfile, `--show-output`:

- `in_memory` filter → 198 tests, 0 failures, 0.385s wall.
- `_fs` filter → 36 tests, 0 failures, 0.418s wall.

In both runs the testcases are self-closing (no captured output)
because these blob tests are quiet on success; what's being
demonstrated is that the binary no longer falls back to single-
threaded mode under capture. (The oplog binary above provides the
complementary proof that *non-empty* captured output is correctly
attributed per test, not mixed across the 4 workers.)

All other touched binaries (`golem-shard-manager`,
`golem-registry-service`, `golem-debugging-service`,
`golem-worker-service/tests/oidc`, `golem-worker-executor/tests/*`,
`integration-tests/tests/*`, `cli/golem-cli/tests`) still contain
at least one `Shared` dep (Docker containers,
`WorkerExecutorTestDependencies`, `EnvBasedTestDependencies`,
`SpawnedRedis*`, `GeneratedPackage`, etc.) and therefore still fall
back to single-threaded execution under capture. They are
*compile*-validated (`cargo check --tests` is green) but not
*capture-parallel*-validated yet — that will happen as 3.2–3.5
land. The `PerWorker` annotations applied here are preparatory
work that becomes effective in those later phases when the
remaining `Shared` deps in each binary get migrated.

Ran the proof binary under capture-on, parallel:

```
cargo test --lib -p golem-worker-executor --no-run
./target/debug/deps/golem_worker_executor-<hash> \
    oplog::tests:: \
    --test-threads=4 \
    --format junit --logfile /tmp/wx-junit.xml \
    --show-output
```

- 404 tests, 0 failures, 0 errors, 14.52s wall-clock with 4 workers.
- Per-test `<system-out>` blocks in the JUnit log contain each test's
  own `tracing` output, correctly attributed and not mixed across
  tests (verified on `read_from_archive`, `blob_read_from_archive`,
  `scheduled_archive`, `write_after_archive`, etc.).

This is the same kind of structured-reporter validation we used in
Phase 2 — proves capture genuinely works under parallel workers, not
just that "no output leaked to the parent stdout".

### 3.2 — `Cloneable` for pure-data deps (1A)

- `IndexedStorageNamespaces` (`ns`, `ns2`) — add serde derives.
- `BlobStorageNamespace` variants — add serde derives.
- `Arc<dyn TestContext>` (4 deps across `agent_config` modules) — replace
  `dyn` with a tag enum; reconstruct on the worker side.

#### 3.2 — Status (DONE)

Phase 3.2 was **re-scoped** from the originally planned `Cloneable`
implementation to explicit `PerWorker` annotation for every targeted
dep. Two notes on terminology: `serde` was only ever one possible
convenience path for `Cloneable` wire encoding, never a hard test-r
requirement; and "trivially cheap" is accurate for the namespace and
`TestContext` deps but is a simplification for the WIT resolve and the
RDBMS service (see per-class reasoning below).

Per-class reasoning:

1. **Storage namespace deps** (`IndexedStorageNamespaces`,
   `BlobStorageNamespace`, `KeyValueStorageNamespace`): the constructors
   are trivially cheap (return wrapper values built from
   `ComponentId::new()` or similar). `PerWorker` is not just equivalent
   to `Cloneable` here — it is arguably **better** for future worker
   isolation, because worker-local fresh IDs reduce cross-worker
   collision risk once the surrounding infra deps stop being `Shared`.
   No reason to add a wire format or serde derives.

2. **`Arc<dyn TestContext>` deps** (`test_context_ts`, `test_context_rust`):
   thin `Arc` wrappers around zero-sized read-only trait objects with no
   IO, no global registration, no external handles. `PerWorker` is
   trivially fine. The dyn→enum refactor would touch every
   `define_matrix_dimension!(lang: Arc<dyn TestContext> -> ...)`
   declaration and every test signature in `agent_config/` (7 files).
   That refactor has merit as an independent architectural cleanup but
   is not justified *as* a test-r migration step.

3. **`SharedAnalysedTypeResolve`** (`golem_host_analysed_type_resolve`):
   the constructor does real work — `AnalysedTypeResolve::from_wit_directory("../wit")`
   reads checked-in WIT files and builds an in-memory resolve. There is
   no obvious existing wire representation for the parsed form, so a
   "Cloneable" path that ships only the source path would still re-read
   and re-parse on every worker — no win over `PerWorker`. The
   containing binary is also still gated by
   `WorkerExecutorTestDependencies` (3.4), so binary-level parallelism
   isn't unlocked by this dep yet either way. Revisit only if worker
   startup profiling after 3.4 shows WIT parsing as a hot spot.

4. **`rdbms_service`** (`RdbmsServiceDefault`): not pure data —
   construction does **not** open DB connections (those are lazy via
   `SqlxRdbms::get_or_create`) but it does build internal caches and
   spawn background eviction tasks via `Cache::new(...)`. Those tasks
   are aborted on drop, so this is not a leak. Safe to construct per
   worker, just not zero-cost; revisit if large worker counts make
   per-worker setup show up in profiles.

Annotations applied in Phase 3.2:

- `golem-worker-executor/tests/indexed_storage.rs` — `ns`, `ns2`.
- `golem-worker-executor/tests/key_value_storage.rs` — `ns`, `ns2`.
- `golem-worker-executor/tests/lib.rs` — `golem_host_analysed_type_resolve`
  (`SharedAnalysedTypeResolve` parsed from `../wit`).
- `golem-worker-executor/tests/rdbms_service.rs` — `rdbms_service`
  (lazy `RdbmsServiceDefault`; spawns background eviction tasks but no
  eager IO).
- `integration-tests/tests/agent_config/mod.rs` — `test_context_ts`,
  `test_context_rust`.
- `integration-tests/tests/agent_config_live_mutation.rs` —
  `test_context_ts`, `test_context_rust`.
- `integration-tests/tests/sharding.rs` — `tracing` (trivially cheap;
  enclosing suite is `#[test_r::sequential]`, so this has no immediate
  parallelism payoff but keeps the dep classification uniform).

Note: `BlobStorageNamespace` deps in `golem-service-base/tests/blob_storage.rs`
(`cc`, `co`, `cs`) were already migrated to `PerWorker` in Phase 3.1 — they
qualify under the storage-namespace reasoning above and 3.1 picked them up
because they sit in a file with other trivially-`PerWorker` factories.

Deferred (not done in 3.2 and intentionally not in scope):

- Adding `serde` derives to `KeyValueStorageNamespace`,
  `IndexedStorageNamespace`, `IndexedStorageMetaNamespace`,
  `BlobStorageNamespace`. No demonstrated production need; existing
  `Debug` already covers logging/debugging; would expand the public
  contract surface of core enums for a test-only benefit.
- Refactoring `Arc<dyn TestContext>` into a shared tag enum.
  Architecturally interesting but should be motivated by the
  `agent_config` matrix typing itself, not by a test-r migration.

##### Compile validation

`cargo check --tests -p golem-worker-executor -p integration-tests` is
green, and `cargo fmt --all -- --check` is clean.

##### Capture+parallel validation

No new fully-non-`Shared` binaries are produced by 3.2. All touched
binaries still contain at least one `Shared` dep — the matrix tests
in `golem-worker-executor` are gated on `WorkerExecutorTestDependencies`
(3.4) and the `agent_config*` tests are gated on `EnvBasedTestDependencies`
(3.5). The 3.2 annotations are preparatory: they become effective
parallelism-enablers when 3.4/3.5 migrate the remaining `Shared` deps in
those binaries.

The Phase 3.1 capture+parallel proofs (oplog + blob_storage) still hold
unchanged.

### 3.3 — `LastUniqueId` refactor (1A)

Seed each worker's counter with `worker_idx << 8` (test-r exposes
`worker_index()` as a helper) and annotate `PerWorker`. Falls back to a
`Hosted` "next id" RPC in 1B only if the 8-bit range proves too narrow.

#### 3.3 — Status (DONE)

Phase 3.3 shipped in two parts:

**test-r core (new 1A surface)**

- New CLI flag `--worker-index <N>` on `Arguments` (hidden, like `--ipc`),
  with matching `to_args()` serialisation.
- Parent runner stamps each test-thread's args with `worker_idx = 0..threads`
  before that thread spawns its worker subprocess (both sync and tokio
  paths). The parent process itself never observes the field.
- New `test_r_core::worker` module backed by `OnceLock<usize>`:
  `set_worker_index(idx)` (called from each runner entry point when
  `--worker-index` is present) and `worker_index() -> usize`
  (defaults to 0 when unset — i.e. in the parent and in any
  no-spawn-workers path).
- Re-exported as `test_r::worker_index` from the umbrella crate.
- Documented in `book/src/advanced_features/dependency_sharing.md` under
  the `PerWorker` section, with the canonical
  `LastUniqueId { (worker_index() << 8) }` recipe.
- New example: `example/src/sharing/per_worker_index.rs` with four
  `PerWorker` tests asserting that each worker's id namespace stays
  within its own high-byte slot.
- New unit test: `test_r_core::worker::tests::defaults_to_zero_when_unset`.
- Capture+parallel JUnit run on the example proves end-to-end behaviour:
  with `--test-threads 4 --format junit --show-output`, the four
  `worker_index_seeds_namespace_*` tests are scheduled on indices 0..3
  and each produces `LastUniqueId(seed=0x0N00, worker_idx=N)` in
  `<system-out>` — direct evidence that the index is reaching the
  PerWorker constructor in a spawned subprocess.

**golem**

- `golem-worker-executor/tests/lib.rs::last_unique_id` →
  `scope = PerWorker`, seeded with `u16::from(u8::try_from(worker_index())?) << 8`.
- `golem-debugging-service/tests/lib.rs::last_unique_id` → same.

Per the oracle review the seed casts use `u8::try_from(...)` rather than
`as u8` so that running with `--test-threads > 256` panics with a clear
message instead of silently aliasing two workers into the same id slot.
The remaining 8 bits per worker accommodate up to 256 ids per worker,
which covers every `TestContext::new` allocation in the golem suites.
The `Hosted` fallback (HR3.1) stays available if a future test pattern
ever needs more than that.

**Oracle-review polish applied**

- `set_worker_index` is `pub(crate)` (user code cannot poison the
  `OnceLock` from outside).
- `worker_index()` rustdoc explicitly notes that it is a process-level
  identifier and that `Shared` / `Cloneable` / `Hosted` / `HostedRpc`
  constructors always observe `0` because they only ever run in the
  parent.
- Golem call sites use `u8::try_from(...)` for fail-fast over silent
  truncation.
- Extra args round-trip regression tests:
  `worker_index_round_trips_through_to_args_and_parse` and
  `worker_index_absent_round_trip_stays_none` in `args.rs::tests`.

No new fully-non-`Shared` binaries emerge from 3.3 in isolation —
`test_dependencies` (`WorkerExecutorTestDependencies`) is still
`Shared` in both binaries until 3.4 lands. The 3.3 annotation
just removes the `LastUniqueId` row from the per-binary `Shared`
inventory.

`cargo build --all-features` and `cargo clippy --no-deps --all-targets
--all-features -- -Dwarnings` are clean on test-r;
`cargo check --tests -p golem-worker-executor -p golem-debugging-service`
and `cargo fmt --all -- --check` are clean on golem.

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

#### 3.4 — Status (DONE as option (c): Hosted fixture-cluster pattern)

Per the second-pass oracle review, 3.4 was re-scoped from "make
`PrecompiledComponent` Cloneable" to "land the Hosted fixture-cluster
pattern by migrating `WorkerExecutorTestDependencies` first". Reasoning:
the parallelism unlock comes from making the cluster Hosted (workers share
the on-disk cache via descriptor), not from making the tiny returned
`PrecompiledComponent` (two strings) Cloneable. `test_component!` and
`PrecompiledComponent` stay `Shared`; the expensive warmup keeps running
parent-side, exactly once, just like before.

**Worker-side reconstruction primitives (golem)**

- `golem-service-base/src/storage/blob/fs.rs::FileSystemBlobStorage::attach_existing(root)`
  — synchronous "attach to a parent-prepared root" constructor. Does NOT
  create the root, `compilation_cache`, or `custom_data` subdirectories
  (the parent already did). Uses `std::fs::canonicalize` so it can be
  called from a sync `HostedDep::from_descriptor`.
- `golem-worker-executor-test-utils/src/component_writer.rs::FileSystemComponentWriter::attach_existing(root)`
  — sync alternative to `new(root)` that does **not** call
  `remove_dir_all(root)`. The parent's already-warmed on-disk component
  store survives every worker startup.
- `golem-test-framework/src/components/redis_monitor/provided.rs::ProvidedRedisMonitor`
  — no-op `RedisMonitor` for workers. The parent's `SpawnedRedisMonitor`
  remains the source of truth; workers only need a value that satisfies
  the `assert_valid()`/`kill()` API.
- `golem-worker-executor-test-utils/src/lib.rs::TestTempDir` — small
  `enum { Owned(TempDir), Borrowed(PathBuf) }` wrapper that exposes the
  same `path()` API in both cases. Dropping `Borrowed` is a no-op, which
  is exactly the safety property a worker needs when it points at the
  parent's `TempDir`s.

**Hosted impl**

- `impl HostedDep for WorkerExecutorTestDependencies` in
  `golem-worker-executor-test-utils/src/lib.rs`:
  - `descriptor()` serialises `{ redis_host, redis_port, redis_prefix,
    blob_storage_root, component_directory, component_service_directory,
    component_temp_directory, data_dir_path }` via `serde_json`.
  - `from_descriptor()` rebuilds the same struct using `ProvidedRedis`,
    `ProvidedRedisMonitor`, `FileSystemBlobStorage::attach_existing`,
    `FileSystemComponentWriter::attach_existing`, and
    `TestTempDir::Borrowed`.
- `test-r` moved from `[dev-dependencies]` to `[dependencies]` in
  `golem-worker-executor-test-utils/Cargo.toml` so the production lib can
  reference `test_r::core::HostedDep`.

**Factory call sites**

- `golem-worker-executor/tests/lib.rs::test_dependencies` →
  `#[test_dep(scope = Hosted)]`, no longer takes `&Tracing` (Hosted owner
  constructors cannot depend on other test_deps). Tracing remains a
  separate `PerWorker` dep installed inside each worker subprocess.
- `golem-debugging-service/tests/lib.rs::test_dependencies` → same.

**Tests**

- `golem-worker-executor-test-utils/src/lib.rs::hosted_descriptor_tests`:
  - `worker_side_drop_does_not_delete_parent_temp_dirs` — round-trips a
    descriptor through `from_descriptor`, drops the resulting struct,
    asserts the parent's `TempDir`s and on-disk subdirs still exist.
  - `worker_side_attach_does_not_destroy_component_service_directory`
    — writes a sentinel file under the parent's
    `component_service_directory`, reconstructs the worker side, asserts
    the sentinel survives (regression for the "must not call `new()`,
    which `remove_dir_all`s" footgun).
- Both green under `cargo test -p golem-worker-executor-test-utils --lib
  hosted_descriptor`.

**Hardening from the oracle review (folded into 3.4)**

The oracle review of the original 3.4 implementation flagged two
brittleness points that we fixed before declaring 3.4 done:

1. **`FileSystemComponentWriter::attach_existing` now fails fast.** It
   `std::fs::canonicalize`s the descriptor path before storing it; if
   the parent-prepared directory doesn't exist, the worker panics
   immediately instead of silently creating a fresh per-worker store
   on the first write (which would have re-introduced the exact
   regression Phase 3.4 was meant to prevent).
2. **`WorkerExecutorTestDependencies::descriptor()` canonicalizes
   every on-disk path before serialisation.** Workers may be spawned
   with a different cwd than the parent, so the descriptor must not
   carry relative paths like `../test-components`. All five path
   fields (`blob_storage_root`, `component_directory`,
   `component_service_directory`, `component_temp_directory`,
   `data_dir_path`) are absolute by the time they leave the parent.

A third regression test pins the first invariant:

- `hosted_descriptor_tests::attach_existing_fails_fast_when_component_dir_missing`
  — calls `FileSystemComponentWriter::attach_existing` on a
  non-existent path inside `catch_unwind` and asserts that it panics.

**Validation**

- `cargo check --tests` across the whole golem workspace — clean.
- `cargo fmt --all -- --check` — clean.
- `cargo clippy -p golem-worker-executor-test-utils --no-deps
  --all-targets -- -Dwarnings` — clean (after hardening).
- `cargo test -p golem-worker-executor-test-utils --lib
  hosted_descriptor` — 3/3 passed.
- Capture+parallel of the `golem-worker-executor` integration binary
  isn't run from this phase: it spawns a real Redis on port 6379, which
  collides with any locally-running Redis, and the suite is long enough
  to hit network/service flakiness in unattended runs. The in-process
  unit tests above pin the correctness invariants this phase introduces.

**What 3.4 unlocked vs what's still gated** (per oracle review)

Phase 3.4 lands the Hosted fixture-cluster *pattern* and removes
`WorkerExecutorTestDependencies` itself as a `Shared` row. It does
**not** by itself unlock parallel-under-capture for the
`golem-worker-executor --test integration` binary, because under
current `test-r` semantics any remaining `Shared` dep in scope still
forces single-thread fallback. The binary still contains `Shared`
rows:

- the 14 `PrecompiledComponent` instances generated by
  `test_component!`,
- the per-component `#[test_dep] pub async fn ...
  (deps: &WorkerExecutorTestDependencies)` factories,
- and other default-`Shared` fixtures wired into the same integration
  binary (`postgres`, `mysql`, `ignite_rdb`, the storage fixtures in
  `tests/indexed_storage.rs` / `tests/key_value_storage.rs`, etc.).

Those are deliberately still `Shared`: the expensive warming
side-effects landed in the parent-owned `component_service_directory`
exactly once, and workers borrow that on-disk state via the Hosted
descriptor. But the rollout claim is now precise: **3.4 unlocks the
ownership model, not the whole-binary capture-parallel run.**

The `golem-debugging-service` integration binary looks structurally
ready for capture-parallel (Hosted + PerWorker only, per oracle
inspection), pending the live verification in Phase 3.6.

The whole-binary parallel-under-capture verification for both
worker-executor and debugging-service is deferred to Phase 3.6, after
3.5 lands `Hosted` for `EnvBasedTestDependencies`. Migrating the
remaining `Shared` `PrecompiledComponent`/factory rows is tracked as
a follow-up alongside 3.6 if the live run still falls back to serial.

### 3.5 — `Hosted` for `EnvBasedTestDependencies` and Docker containers (1B)

- `EnvBasedTestDependencies` — owner constructor stays; new
  `EnvBasedTestDependenciesClient::from_descriptor(desc)` re-opens gRPC
  clients from descriptor addresses.
- Docker container deps — descriptor is `{host, port}`. Some have richer
  surfaces (DB credentials), still small enough.
- `HttpTestContext` / `McpTestContext` become `PerWorker` deps consuming the
  Hosted cluster client.

#### 3.5.0 — `AsyncHostedDep` + `async_worker` (test-r upstream prerequisite, DONE)

Worker-side reconstruction for `EnvBasedTestDependencies` (golem) has
to call async `Provided*::new(...).await` constructors of subordinate
services — `ProvidedWorkerService::new`, `ProvidedRegistryService::new`,
etc. — but `HostedDep::from_descriptor` is sync. Rather than coerce
every `Provided*::new` to be sync just so the worker reconstructor can
call it (the original 3.4 hardening plan), 3.5 lands native async
support in test-r itself.

**Trait** — `test_r::core::AsyncHostedDep`:

```rust
pub trait AsyncHostedDep: Sized + Send + Sync + 'static {
    fn descriptor(&self) -> Vec<u8>;
    fn from_descriptor(bytes: &[u8]) -> impl std::future::Future<Output = Self> + Send;
}
```

`descriptor()` stays sync (it runs on the parent owner, exactly as in
`HostedDep`); only `from_descriptor` becomes async. Implementors choose
one trait or the other per dep — the two are mutually exclusive.

**Macro flag** — `#[test_dep(scope = Hosted, async_worker)]` opts a
single dep into the async worker reconstruction path:

- Routes `descriptor()` through `AsyncHostedDep::descriptor` and the
  worker reconstructor through `AsyncHostedDep::from_descriptor(...).await`.
- Emits a `WorkerReconstructor::Async` closure (the test-r runtime
  already supported this enum variant since 1B — it's the same path
  used by async `Cloneable` reconstructors).
- Rejected for any scope other than `Hosted`.
- Requires the `tokio` runtime feature on the consumer crate (the sync
  runner cannot drive async worker reconstructors; same restriction
  that already applies to async `#[test_dep]` constructors).

**Files touched**

- `test-r-core/src/internal.rs` — new `AsyncHostedDep` trait with full
  doc comment cross-referencing `HostedDep`.
- `test-r/src/lib.rs` — re-export `AsyncHostedDep` at the top level and
  under `test_r::core`.
- `test-r-macro/src/deps.rs`:
  - new `async_worker: bool` on `TestDepArgs`,
  - validation that rejects the flag for any scope other than `Hosted`,
  - the existing Hosted codec/worker-fn block now switches both the
    parent-side `descriptor()` trait path and the worker-side
    reconstructor between sync (`HostedDep` + `WorkerReconstructor::Sync`)
    and async (`AsyncHostedDep` + `WorkerReconstructor::Async`) based on
    the flag.
- `example-tokio/src/sharing/hosted_async_worker.rs` — new example
  mirroring `hosted_basic.rs` but using `AsyncHostedDep`. Worker side
  `.await`s on `TcpStream::connect` inside `from_descriptor`, stores a
  prewarmed client in `Option<Mutex<TcpStream>>`, and four tests verify
  the wiring (fresh round-trip, prewarmed round-trip, owner-runs-once
  in parent, worker reconstructor actually runs in worker).
- `example-tokio/src/sharing/mod.rs` — module registration.
- `book/src/advanced_features/dependency_sharing.md` — new
  "Async worker-side reconstruction (`AsyncHostedDep`)" subsection
  under "Hosted" with a worked example and the `async_worker`
  restrictions.

**Mode-consistent fallback (oracle review fix)**

The oracle review of 3.5.0 caught a runtime inconsistency:
`test-r-core/src/tokio.rs::apply_hosted_descriptors_locally` (the
no-spawn-workers fallback that pre-populates Hosted handles directly
in the parent's `TestSuiteExecution`) was sync-only and explicitly
panicked on `WorkerReconstructor::Async`. That contradicted the
documented mode-consistent Hosted contract in the book.

Fixed by:

- making `apply_hosted_descriptors_locally` async and matching on both
  `WorkerReconstructor::Sync` and `WorkerReconstructor::Async`,
- `.await`ing the call site in the top-level parent's
  no-spawn-workers branch.

The `hosted_async_worker` example was updated to match: the
`async_hosted_round_trip_prewarmed_connection` and
`async_hosted_worker_reconstructor_runs_in_worker` tests now expect the
prewarmed worker-side handle in **both** the spawned-worker path and
the no-spawn fallback, exercising the mode-consistent semantics.

**Validation**

- `cargo build -p test-r-example-tokio --tests` — clean.
- `cargo test -p test-r-example-tokio --lib sharing::hosted_async_worker
  -- --spawn-workers --test-threads 2` — 4/4 passed.
- `cargo test -p test-r-example-tokio --lib sharing::hosted_async_worker`
  (no-spawn-workers fallback) — 4/4 passed.
- `cargo test -p test-r-example-tokio --lib sharing::hosted_basic`
  (sync Hosted regression) — 4/4 passed.
- `cargo clippy --no-deps --all-targets -- -Dwarnings` — clean.
- `cargo fmt --all -- --check` — clean.
- `cargo test -p test-r --all-features` — green (the only failing test
  in that run is the intentionally-broken `it_does_work` self-test).

#### 3.5.1 — First `EnvBasedTestDependencies` Hosted consumer migration (DONE)

Per the oracle's narrower-scope recommendation, the first 3.5 consumer
migration uses **only** `integration-tests/tests/agent_config_live_mutation.rs`.
The remaining integration binaries (in particular `integration-tests/tests/lib.rs`
which pulls in `plugins.rs` / `sharding.rs` calling cluster lifecycle methods
like `kill_all` / `restart_all` / `stop` / `start`) stay on `Shared` for now —
they need either the future HostedRpc control-plane or a parent-owned
fallback before they can move.

**Files touched (golem repo)**

- `golem-test-framework/Cargo.toml` — promoted `test-r` from
  `[dev-dependencies]` to `[dependencies]` because library code now
  implements `test_r::core::AsyncHostedDep`.
- `golem-test-framework/src/components/rdb/borrowed_sqlite.rs` — new
  non-owning `BorrowedSqliteRdb` Rdb handle. `new()` only logs and
  records the path; `kill()` and `Drop` are no-ops so a worker
  reconstruction never deletes the parent-owned SQLite database.
- `golem-test-framework/src/components/rdb/mod.rs` — `pub mod borrowed_sqlite;`.
- `golem-test-framework/src/components/redis_monitor/provided.rs` — new
  no-op `ProvidedRedisMonitor` so existing `deps.redis_monitor().assert_valid()`
  callers keep compiling on the worker side.
- `golem-test-framework/src/components/worker_executor_cluster/provided.rs` —
  `ProvidedWorkerExecutorCluster::from_endpoints(...)` reconstructs a
  multi-member cluster handle backed by `ProvidedWorkerExecutor`. The
  legacy single-endpoint `new(host, grpc_port)` is preserved for the
  existing benchmark caller. All lifecycle methods (`kill_all`,
  `restart_all`, `stop(i)`, `start(i)`) explicitly **panic** with an
  actionable message — the worker side has no authority over
  parent-owned worker-executor processes; tests that need lifecycle
  control must keep `Shared` (or migrate via a future HostedRpc
  control plane).
- `golem-test-framework/src/config/env.rs`:
  - new `TestTempDir { Owned(TempDir), Borrowed(PathBuf) }` wrapper.
    `EnvBasedTestDependencies::temp_directory` is now
    `Arc<TestTempDir>`. Parent path uses `Owned`; worker
    reconstruction uses `Borrowed` so worker-side drop never deletes
    the parent's tree.
  - new `EnvBasedTestDependenciesDescriptor` (+ nested per-service
    descriptors for RDB / shard manager / component compilation /
    worker service / worker executor cluster / registry service).
  - `impl test_r::core::AsyncHostedDep for EnvBasedTestDependencies`:
    parent `descriptor()` serialises canonicalised absolute paths
    (temp dir, blob storage root, test component dir) + endpoints +
    registry metadata + Redis/RDB info, mirroring the
    `WorkerExecutorTestDependencies` hardening. Worker
    `from_descriptor(...).await` reconstructs `ProvidedRedis`,
    `ProvidedRedisMonitor`, either `ProvidedPostgresRdb` or
    `BorrowedSqliteRdb` (RDB type chosen from the descriptor),
    `ProvidedShardManager`, `ProvidedComponentCompilationService`,
    `ProvidedRegistryService`, `ProvidedWorkerService`, and the
    multi-member `ProvidedWorkerExecutorCluster`. Blob storage attaches
    to the parent's existing FS root rather than recreating it.
- `integration-tests/tests/agent_config_live_mutation.rs` —
  `create_deps` switched from plain `#[test_dep] async fn …(_tracing:
  &Tracing) -> EnvBasedTestDependencies` to `#[test_dep(scope = Hosted,
  async_worker)] pub async fn create_deps() -> EnvBasedTestDependencies`.
  Hosted owner constructors cannot depend on other test_deps so the
  `&Tracing` parameter is dropped; `tracing()` remains a separate
  `PerWorker` dep installed inside each worker subprocess. Inline
  comment explains why this binary is safe to migrate today and the
  remaining binaries are not.

**Phase-internal hygiene**

- `golem-test-framework/src/config/env.rs`,
  `golem-test-framework/src/components/redis_monitor/provided.rs`,
  `golem-worker-executor/tests/lib.rs` — dropped the internal "Phase 3.4"
  marker from comments. Downstream code should not reference
  NOTES.md-internal phase numbers (the user explicitly asked for this
  during the wasm-rquickjs migration and the same applies here).

**Unit tests added in `golem-test-framework`** (all under `#[cfg(test)]`,
discovered by the existing `test_r::enable!()` in `lib.rs`):

- `components::rdb::borrowed_sqlite::tests`
  - `info_round_trips_path` — `Rdb::info()` returns the original path.
  - `drop_does_not_delete_underlying_path` — `Drop` is non-destructive
    even when the file exists.
  - `kill_is_a_no_op` — `Rdb::kill()` is non-destructive.
- `components::worker_executor_cluster::provided::tests`
  - `legacy_single_constructor_reports_size_one` — `new(host, port)`
    keeps `size() == 1` for the existing benchmark caller.
  - `from_endpoints_preserves_size_and_indices` — multi-member
    constructor reports the configured size, `to_vec()` length,
    `is_running`, `started_indices`, `stopped_indices`.
  - `kill_all_panics_on_worker_side`, `restart_all_panics_on_worker_side`,
    `stop_panics_on_worker_side`, `start_panics_on_worker_side` — each
    asserts the expected actionable panic message ("kill_all is
    unsupported", "stop(1) is unsupported", etc.).
- `config::env::tests`
  - `owned_drop_deletes_directory` — `TestTempDir::Owned` still cleans
    up its `TempDir` on drop.
  - `borrowed_drop_does_not_delete_directory` — `TestTempDir::Borrowed`
    leaves the parent-owned tree alone on drop.
  - `descriptor_serde_round_trip_sqlite`,
    `descriptor_serde_round_trip_postgres` — serialize → deserialize →
    re-serialize on a fully-populated `EnvBasedTestDependenciesDescriptor`
    preserves every field (paths, ports, hosts, registry metadata,
    multi-member worker-executor list) and yields a stable byte
    sequence. Added per oracle follow-up to lock the wire format
    before further consumer migrations.
  - `descriptor_sqlite_uses_snake_case_kind_tag`,
    `descriptor_postgres_uses_snake_case_kind_tag` — RDB variant tag is
    `kind: "sqlite"` / `kind: "postgres"`; renaming an `RdbDescriptor`
    variant in source becomes a visible breaking change to the
    cross-process descriptor exchange.
  - `descriptor_canonical_helper_yields_absolute_paths` — mirrors what
    `AsyncHostedDep::descriptor` does: relative path through
    `std::fs::canonicalize` is absolute, so worker subprocesses running
    with a different cwd than the parent can still resolve it.

**Validation**

- `cargo fmt --all -- --check` — clean.
- `cargo clippy -p golem-test-framework --no-deps --all-targets -- -Dwarnings` — clean.
- `cargo clippy -p integration-tests --tests --no-deps -- -Dwarnings` — clean.
- `cargo test -p golem-test-framework --lib` — 16/16 passed (11
  worker-side-safety tests + 5 descriptor round-trip tests added per
  oracle follow-up).
- `cargo test --no-run -p integration-tests --test agent-config-live-mutation` — builds.

Runtime validation of the live binary against a real cluster is
intentionally **not** in this sub-step: it requires the full
worker-executor cluster + Redis + RDB stack and a free port 6379,
which this machine already occupies. The compile + targeted unit
tests are sufficient evidence for the worker-side safety properties
this sub-step introduces; full runtime sign-off happens in 3.6.

**Oracle review** — landed with the recommendation to add a
`EnvBasedTestDependenciesDescriptor` serde round-trip test before the
next consumer migration; addressed by the five `descriptor_*` unit
tests listed above. Oracle's audit checklist for the next consumer
(`integration-tests` binaries that may not yet be safe to migrate):
search the candidate test for direct/indirect lifecycle use of
`worker_executor_cluster().kill_all/restart_all/stop/start`, of
`worker_executor_cluster().to_vec()[i].kill()/restart()`, of
`shard_manager().kill()/restart()`, of `worker_executor_cluster()`
state queries `started_indices/stopped_indices/is_running`, and of
`.kill()` on worker service / registry / redis / compilation service.
Each of these forces staying on `Shared` until a HostedRpc control
plane (Phase 1C / HR3.2) is available.

### 3.6 — Suite-by-suite verification (DONE — explicit-scope sweep)

The "Phase 3 exit criteria" require **every `#[test_dep]` in the golem
repo to use an explicit scope**. Rather than land nine separate
suite-by-suite PRs each making a different scope choice, 3.6 lands as
one mechanical sweep that:

- annotates every previously-inferred-scope `#[test_dep]` (40 macro
  call sites + 1 macro-rules expansion in
  `golem-worker-executor-test-utils/src/lib.rs::test_component!`) as
  **explicit `scope = Shared`**;
- preserves exact current behaviour at every site (the inferred
  default was already `Shared`, so this is a semantic no-op);
- yields a clean grep for the exit criterion: `rg -nP '#\[test_dep[^\]]*\]' --type rust`
  in the golem repo returns nothing without `scope =`.

This was chosen over the originally drafted 9-PR cascade for two
reasons:

1. The non-trivial scope upgrades already happened in their own
   phases — `PerWorker` in 3.1/3.2/3.3, `Cloneable` in 3.4-as-shipped,
   `Hosted` for `WorkerExecutorTestDependencies` in 3.4 and the first
   `EnvBasedTestDependencies` consumer in 3.5.1. What was left was
   "make the remaining defaults explicit", which has no per-suite
   judgment to make.
2. The remaining `Shared` deps fall into clear categories that
   `Shared` is still the correct answer for today:
   - **Docker container deps** (`DockerJaeger`, `DockerOtelCollector`,
     `DockerPostgresRdb`, `DockerMysqlRdb`, `DockerIgniteRdb`,
     OIDC test servers) — singletons that must not be multiplied per
     worker. Future `Hosted`/`HostedRpc` migration is HR3.2.
   - **`EnvBasedTestDependencies` parents** for binaries that still
     call worker-executor / shard-manager lifecycle methods
     (`integration-tests/tests/lib.rs`, `sharding.rs`,
     `plugins.rs`, `worker.rs`, etc.). These cannot become `Hosted`
     until the HostedRpc control-plane lands (Phase 1C / HR3.3).
   - **Storage-namespace tagged deps** (`indexed_storage`,
     `key_value_storage`) that consume `WorkerExecutorTestDependencies`
     and parameterise on an in-memory / redis / sqlite / postgres
     backend. These already sit under a `Hosted` root (the
     `WorkerExecutorTestDependencies` migrated in 3.4), so leaving
     them `Shared` is a behaviour-preserving choice that does not
     block capture-parallelism — they are no longer the gating
     factor. Future per-site optimisation is optional, not required.
   - **`bridge_gen` `GeneratedPackage` deps** in `cli/golem-cli` —
     migrated to `PerWorker` (not `Shared`) because each worker
     produces its own on-disk generated package and there is no
     parent value to share. This was the one suite where 3.6
     actually changes a scope; see "Per-site exceptions" below.

**Files touched (one explicit scope per macro call)**

The sweep used regex replacements over `--type rust`:

- `#[test_dep]` → `#[test_dep(scope = Shared)]`
- `#[test_dep(tagged_as = "X")]` → `#[test_dep(scope = Shared, tagged_as = "X")]`

Affected files (grep-derived):

- `cli/golem-cli/tests/bridge_gen/{rust,typescript}.rs` —
  `scope = PerWorker` (per-worker on-disk generated packages).
- `integration-tests/tests/lib.rs` — `EnvBasedTestDependencies` parent
  (binary calls cluster lifecycle methods → must stay `Shared`).
- `integration-tests/tests/sharding.rs` — same parent + same reason.
- `integration-tests/tests/otlp_plugin.rs` —
  `DockerJaeger` + `DockerOtelCollector`.
- `integration-tests/tests/custom_api/*.rs` (`mcp.rs`,
  `openapi_generation.rs`, `agent_http_principal_ts.rs`,
  `agent_http_routes_{ts,rust}.rs`) — `HttpTestContext` / `McpTestContext`
  derived from the binary's `Shared` `EnvBasedTestDependencies`.
- `golem-worker-executor/tests/rdbms.rs`, `rdbms_service.rs`,
  `ignite_service.rs` — Docker `Postgres` / `Mysql` / `Ignite` rdb
  fixtures.
- `golem-worker-executor/tests/indexed_storage.rs`,
  `key_value_storage.rs` — backend matrix deps consumed under the
  Hosted parent.
- `golem-shard-manager/tests/persistence.rs` — sqlite / postgres
  backends used by the shard-manager persistence matrix.
- `golem-registry-service/tests/repo/postgres.rs` —
  `postgres` / `postgres_tls` repo deps.
- `golem-worker-service/tests/oidc/{lib,handler}.rs` — OIDC servers
  and their TLS / Redis variants.
- `golem-worker-executor-test-utils/src/lib.rs::test_component!` —
  `#[test_dep(tagged_as = $tag)]` inside the macro body bumped to
  `scope = Shared`; this means every `test_component!(...)` invocation
  in `golem-worker-executor/tests/*.rs` now expands to an explicit
  `Shared` `PrecompiledComponent` fixture under the Hosted parent.

**Per-site exceptions**

- `cli/golem-cli/tests/bridge_gen/{rust,typescript}.rs`: `GeneratedPackage`
  owns a `TempDir` containing generated TS / Rust source + a compiled
  artefact. There is no parent value to share across workers, and the
  generated tree is the unit under test for each test case. `PerWorker`
  is the correct semantic here: each worker pays one
  `generate_and_compile` per tagged variant. Cloneable was the
  originally drafted choice in the 3.6 plan, but `Cloneable` requires
  the dep type to be **reconstructible in workers** (`from_wire(...) -> Self`),
  which means the `TempDir`-owning representation would need an
  owned-vs-borrowed split similar to `TestTempDir` (parent owns
  cleanup, workers attach a non-owning path handle). That refactor
  also has to touch every consuming test signature — out of scope for
  the mechanical exit-criteria sweep. (Note: `Arc<TempDir>` is **not**
  the right fix; it only helps in-process sharing, not cross-process
  worker reconstruction.)

**Validation**

- `rg -nP '#\[test_dep[^\]]*\]' --type rust | grep -v 'scope ='` —
  empty (exit criterion met).
- `cargo check --tests -p golem-cli -p golem-worker-executor
  -p golem-worker-service -p golem-registry-service -p golem-shard-manager
  -p integration-tests` — clean.
- `cargo fmt --all -- --check` — clean.
- `cargo clippy --no-deps --all-targets -p golem-cli -p golem-shard-manager
  -p golem-worker-service -p golem-registry-service -p golem-worker-executor
  -p golem-worker-executor-test-utils -p integration-tests
  -p golem-test-framework -- -Dwarnings` — clean.
- `cargo test -p golem-test-framework --lib` — 16/16 (the Phase 3.5.1
  worker-side-safety + descriptor round-trip unit tests still pass).

**Exit criteria — status**

| Criterion | Status |
|-----------|--------|
| Every `#[test_dep]` in the golem repo uses an explicit scope | DONE (3.6) |
| Integration test suite runs with `--test-threads ≥ 2` under capture-on | PARTIAL after HR3.3: no `scope = Shared` remains under `integration-tests/tests`, the single-thread fallback warning is gone, and targeted runs prove real overlap. Strict full-suite green is still pending because `integration::worker::get_running_workers` timed out and one full run also saw `integration::otlp_plugin::otlp_basic_trace_export` miss traces. |
| Wall-clock improvement measured on the slowest CI lane and documented | DEFERRED to CI timing (HR3.3 implementation is done locally, but no before/after timing from the slowest golem CI lane has been recorded yet). |

The two non-DONE criteria are NOT regressions. HR3.3 removed the
Shared-dependency scheduling bottleneck, but the final full-suite claim
needs the two unrelated integration-test failures fixed or quarantined,
and the wall-clock claim still needs the slowest golem CI lane.

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

> **Status — DONE (with deferral).** See the "Phase HR1.0 (IPC framing
> regression tests) — DONE" header bullet at the top of this file for
> the actual shipped state. The bullets below were the original
> speculative spec; some details below differ from what shipped:
>
> - Frame direction: the **shipped** frames are
>   `IpcResponse::HostedRpcCall { request_id, dep_id, method_idx, args_bytes }`
>   (worker → parent) and
>   `IpcCommand::HostedRpcReply { request_id, body: HostedRpcReplyBody }`
>   (parent → worker). The original spec below has the
>   command/response direction reversed.
> - The true reader-task / waiter-table multiplexer is **deferred**.
>   The shipped MVP transport holds the worker IPC connection mutex
>   for the full request/response round-trip; concurrent caller
>   attempts (`tokio::join!`, `std::thread`) serialise on that mutex
>   without deadlock under the MVP temporal invariant. The
>   `request_id` field is already on the wire so a future
>   reader-task implementation does not need an additional protocol
>   change.

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

> **Update — Phase HR1.2 — DONE.** The tokio runner now supports
> `HostedRpc` end-to-end on parity with the sync runner. See the
> "Phase HR1.2 (tokio HostedRpc) — DONE" header bullet at the top of
> this file for the summary. The historical Phase 1C section below is
> retained as the record of what originally shipped in Phase 1C; any
> phrasing that says "sync runner only" / "tokio rejects HostedRpc" /
> "tokio explicitly panics" should now be read as "what 1C shipped",
> not "what is in the code today".

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
shared parent state with an explicit owner.

**Decision (closed): won't ship unless triggered.** Phase 2.1's
parent-held `NamedUtf8TempFile` shutdown coordination has shown no
issues across the full wasm-rquickjs suite since it landed; there is
no observed shutdown race or leaked file regression that would
motivate building a `TempFileRegistry` HostedRpc surface. Removed
from the active HR backlog. Re-open only if Phase 2.1 starts
exhibiting concrete lifetime problems in CI.

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

#### HR3.1 — shipped — DONE

Shipped shape (slightly different from the sketch above to avoid touching
~80 `&LastUniqueId` parameters and `inherit_test_dep!(LastUniqueId)`
sites in golem):

- `UniqueIds` trait + `LastUniqueIdOwner` (parent-side `AtomicU64` starting
  at `1`) added to `golem-worker-executor-test-utils`.
- `LastUniqueIdOwner` implements `UniqueIds` and `HostedRpcDep` via the
  macro-generated `UniqueIdsDispatch::dispatch_unique_ids`. The owner
  deliberately does **not** derive `Default` — a manual `Default` impl
  forwards to `new()` so the "never returns 0" contract preserved from
  the previous `AtomicU16`-based shape can't silently regress.
- `pub type LastUniqueId = UniqueIdsStub;` keeps all downstream test sites
  source-compatible (parameters stay `&LastUniqueId`, `inherit_test_dep!`
  still names `LastUniqueId`).
- `TestContext::unique_id` widened from `u16` to `u64`; only consumer is
  the `redis_prefix()` format string, so the widening is safe.
- `golem-debugging-service/tests/lib.rs` and
  `golem-worker-executor/tests/lib.rs` both swap their
  `#[test_dep(scope = PerWorker)] fn last_unique_id()` to
  `#[test_dep(scope = HostedRpc, stub = LastUniqueId)] fn last_unique_id_owner()`,
  deleting the `worker_index << 8` partitioning and the `AtomicU16` import.

Along the way, the `#[hosted_rpc]` macro in `test-r-macro` was extended
to also generate a `Debug` impl for the stub struct (formatting as
`<StubName> { dep_id: ... }`). Without it, downstream HostedRpc adopters
couldn't use stubs as parameters of test fixtures decorated with
`#[tracing::instrument]`-style attributes that require every parameter
to implement `Debug`. Locked in by a new macro unit test
`emits_debug_impl_on_stub`.

Tests added:

- `test-r-macro/src/hosted_rpc.rs::tests::emits_debug_impl_on_stub`
  (macro emits `Debug` for the stub).
- `golem-worker-executor-test-utils/src/lib.rs::last_unique_id_owner_tests`:
  `new_starts_at_one`, `default_starts_at_one`,
  `next_is_strictly_monotonic_and_unique` — pin the never-returns-0 and
  uniqueness contracts so a future "derive Default" regression is caught.

Validation (all green):

- `cargo test -p test-r-macro --lib` — 23/23.
- `cargo test -p test-r-example --lib sharing::hosted_rpc` — 15/15
  (same under `--spawn-workers --test-threads 4`).
- Same on `test-r-example-tokio` — 15/15 in both modes.
- `cargo test -p golem-worker-executor-test-utils --lib` — 6/6.
- `cargo check -p golem-worker-executor-test-utils -p golem-debugging-service -p golem-worker-executor --tests` — clean.
- `cargo clippy --no-deps -p ... --tests --all-features -- -Dwarnings` — clean.
- `cargo fmt --all -- --check` — clean in both repos.

Oracle review: round 1 flagged the silent `Default`-derives-zero hazard;
round 2 (after the fix + the regression tests + the documentation
updates) confirmed HR3.1 closable.

Follow-up explicitly deferred (oracle agreed it's not a blocker): a
Redis/Docker-backed end-to-end migration smoke test for the golem
debugging-service / worker-executor suites once that infra is available
in CI. The framework-level HR1 HostedRpc round-trip coverage plus the
new owner-contract tests provide the substantive guard for now.

### HR3.2.0 — test-r prerequisite: unify Hosted worker-view picker, drop `async_worker`

#### Motivation

Implementing HR3.2 in golem the way we actually want (keep
`EnvBasedTestDependencies`'s descriptor-based reconstruction for the
bulk-data gRPC clients **and** add a HostedRpc control surface for the
small set of operations workers can't do via gRPC) hits two test-r
shape problems:

1. `scope = Hosted` and `scope = HostedRpc` are mutually exclusive
   scopes today. There is no way to register a single dep that has
   **both** a descriptor reconstruction path on workers and an RPC
   stub. Without that, the only way to expose an RPC surface on
   `EnvBasedTestDependencies` is to either replace its descriptor path
   entirely (which sacrifices direct gRPC streaming, doubles
   serialisation, caps concurrency at one in-flight call, etc. — see
   the discussion above) or to bolt on a sibling HostedRpc dep that
   then can't share the parent's container handle (HostedRpc owners
   may not depend on other test_deps).
2. The `async_worker` flag is a syntactic stand-in for "this dep's
   worker-side reconstructor returns a future." It exists because the
   macro can't reflect on which trait (`HostedDep` vs `AsyncHostedDep`)
   the dep's return type implements. It is one more thing the user
   has to remember, easy to forget, and silently no-ops on non-Hosted
   scopes (rejected, but still a footgun).

HR3.2.0 is the test-r upstream change that lifts both restrictions
before we touch golem again. HR3.2 and HR3.3 then ride on top.

#### Design — Option A naming + blanket bridge

##### Macro surface

Replace today's `scope = HostedRpc, stub = X` with a single picker on
the `Hosted` scope. The picker says what shape the **worker side**
sees; the owner always lives in the parent. Valid combinations:

```rust
#[test_dep(scope = Hosted)]
async fn env() -> EnvBasedTestDependencies { … }
// → today's `scope = Hosted` (descriptor-based reconstruction on
//   workers; sync or async chosen automatically — see below)

#[test_dep(scope = Hosted, worker = rpc(UniqueIds))]
fn unique_ids_owner() -> LastUniqueIdOwner { … }
// → today's `scope = HostedRpc, stub = LastUniqueId`. The stub type
//   name comes from `<TraitName>Stub` (the macro-generated name) so
//   `stub = X` is no longer needed.

#[test_dep(scope = Hosted, worker = both(RedisControl))]
async fn env() -> EnvBasedTestDependencies { … }
// → NEW shape (HR3.2 enabler). Workers get BOTH:
//   * the descriptor-reconstructed `EnvBasedTestDependencies` (so all
//     existing gRPC clients keep working), AND
//   * an `&RedisControlStub` for the methods that need parent-owned
//     state. Test fixtures parameterise on whichever they need:
//     `fn t(deps: &EnvBasedTestDependencies, ctrl: &RedisControlStub)`.
```

The owner-side responsibility for `worker = both(T)` is that the
single owner constructor's return type must implement **both**
`HostedDep`/`AsyncHostedDep` (for `descriptor`/`from_descriptor`) **and**
`HostedRpcDep<Stub = TStub>` (for `dispatch`/`build_stub`). The macro
generates both injection arms.

Visually the three forms are easy to tell apart at review time, which
was the user's complaint about `Hosted, rpc = T` vs `HostedRpc, rpc = T`.

##### `async_worker` removal via blanket bridge

The trait pair stays:

```rust
pub trait HostedDep: Sized + Send + Sync + 'static {
    fn descriptor(&self) -> Vec<u8>;
    fn from_descriptor(bytes: &[u8]) -> Self;
}

pub trait AsyncHostedDep: Sized + Send + Sync + 'static {
    fn descriptor(&self) -> Vec<u8>;
    fn from_descriptor(bytes: &[u8]) -> impl Future<Output = Self> + Send;
}
```

Add a blanket impl: every `HostedDep` is also an `AsyncHostedDep`,
wrapping the sync result in `std::future::ready`:

```rust
impl<T: HostedDep> AsyncHostedDep for T {
    fn descriptor(&self) -> Vec<u8> { <T as HostedDep>::descriptor(self) }
    fn from_descriptor(bytes: &[u8]) -> impl Future<Output = Self> + Send {
        std::future::ready(<T as HostedDep>::from_descriptor(bytes))
    }
}
```

Under the `tokio` feature the macro **always** emits the async
reconstruction path (`AsyncHostedDep::from_descriptor(bytes).await`)
for `scope = Hosted` deps. Sync impls are bridged automatically and
the compiler reduces the `ready(...).await` to a direct call, so there
is no real runtime cost.

Under sync-only builds (no `tokio` feature) the async trait does not
exist; the macro keeps emitting `HostedDep::from_descriptor(bytes)`
directly, exactly as today. The `async_worker` flag is unused on the
sync runner already and goes away on the tokio runner.

Net effect:

- `#[test_dep(scope = Hosted, async_worker)]` becomes
  `#[test_dep(scope = Hosted)]`.
- The flag is rejected with a compile-time deprecation message during
  a transition window, then removed.
- Implementors choose between `impl HostedDep` (sync body) and
  `impl AsyncHostedDep` (async body) purely at trait-impl time.

##### Runtime implications

- `WorkerReconstructor::Sync` and `WorkerReconstructor::Async` collapse
  into a single async path on the tokio runner. One `Pin<Box<Future>>`
  allocation per worker-startup-per-dep on what was previously the
  sync path. Test-setup-only cost, irrelevant in practice.
- The `worker = both(T)` registration path needs to install both the
  Hosted descriptor handle **and** the HostedRpc stub on the worker
  side. Today's runtime keeps these in separate registries; this RFC
  adds a small composite registration that wires both for one
  `RegisteredDependency`.
- `HostedRpc` owner restriction ("may not depend on other test_deps")
  stays unchanged — the user-visible "owner constructor runs in the
  parent with an empty dep view" rule is the same on `Hosted` and on
  `Hosted, worker = rpc(T)`/`worker = both(T)`.

##### Migration shape inside test-r

1. Add the blanket `AsyncHostedDep` impl in `test-r-core/src/internal.rs`.
2. Add a new `WorkerView { Descriptor, Rpc(stub), Both(stub) }` parsed
   from the `worker = …` attribute on `scope = Hosted`. Map the legacy
   `scope = HostedRpc, stub = X` to `scope = Hosted, worker = rpc(X)`
   for back-compat (emit a deprecation warning, then remove).
3. Always emit the async-await path on the tokio runner for the
   descriptor reconstructor. Reject `async_worker` with a deprecation
   diagnostic during the transition, then remove the field from
   `TestDepArgs`.
4. For `worker = both(T)`, generate two registration adapters on the
   parent owner cell (one for the descriptor reconstructor, one for
   the HostedRpc dispatcher). The owner-cell collection picks both up.
5. Update worker-side dep view code so a `Both`-shaped dep can be
   injected as either `&Self` (the descriptor type) or `&TStub` (the
   RPC stub) depending on the test-fn parameter type.
6. Examples + docs:
   - rewrite `example/src/sharing/hosted_rpc_*.rs` to use
     `scope = Hosted, worker = rpc(TraitName)`
   - new `example-tokio/src/sharing/hosted_both.rs` exercising
     `worker = both(T)` end-to-end
   - update `book/src/advanced_features/dependency_sharing.md`
     "Hosted" subsection to reflect the new picker + the
     `async_worker` removal
7. Tests:
   - new macro unit tests for each `worker = …` variant + each
     rejection (e.g. `worker = rpc(T)` rejected on non-Hosted scopes,
     `worker = both(T)` requires a type implementing both traits,
     deprecation of `scope = HostedRpc` / `async_worker` during
     transition)
   - integration tests confirming a `worker = both(T)` dep is
     reachable as descriptor handle, as RPC stub, and as both in the
     same fixture
8. NOTES.md / book: deprecation + removal timeline.

#### Out of scope for HR3.2.0

- Changing the HostedRpc transport (still one in-flight per dep, still
  `desert_rust`-coded).
- Allowing HostedRpc owners to depend on other test_deps. The
  `worker = both(T)` shape already satisfies the EnvBasedTestDependencies
  case without needing to lift that restriction.
- Replacing any existing gRPC client path with RPC. HR3.2.0 only
  enables the *coexistence* of descriptor + RPC for one dep.

#### Exit criteria for HR3.2.0

- All existing examples and golem `scope = Hosted [async_worker]` and
  `scope = HostedRpc, stub = X` deps continue to compile and pass
  tests via the back-compat shim.
- The new `worker = both(T)` form works end-to-end in
  `example-tokio/src/sharing/hosted_both.rs` (test under both
  `--spawn-workers` and the no-spawn fallback).
- Oracle review passes.

#### HR3.2.0 Step 1 — blanket bridge `impl<T: HostedDep> AsyncHostedDep for T` — DONE

Shipped scope (intentionally narrow):

- Added the blanket `impl<T: HostedDep> AsyncHostedDep for T` in
  `test-r-core/src/internal.rs` right after the `AsyncHostedDep`
  trait definition. Bridges sync `from_descriptor` to async via
  `std::future::ready(...)`.
- Added a `#[cfg(test)] mod hosted_dep_blanket_bridge_tests` with one
  test (`blanket_impl_exposes_sync_hosted_dep_via_async_api`) that
  compile-time-witnesses `T: AsyncHostedDep` for a sync-only
  `HostedDep` fixture, then drives the bridged future once with a
  hand-rolled no-op `RawWaker` to assert the value comes back
  immediately.
- No macro changes, no runtime changes, no `async_worker` removal
  yet. Step 1 is just the trait-level bridge that the later steps
  ride on.

Doc comment incorporates the oracle's hardening feedback:
- "negligible bridge cost" with the small async-wrapper caveat that
  applies once the runtime routes everything through the async path
  (rather than overstating "no cost").
- Coherence note explicitly says "on stable Rust, rustc rejects
  overlapping manual impls as conflicting implementations."
- Source-compat note about UFCS being needed when downstream code
  imports both traits and uses method syntax that becomes ambiguous.

Verification (all green):

- No existing type in test-r or golem implements both `HostedDep` and
  `AsyncHostedDep` (audited; would have coherence-conflicted with the
  blanket otherwise).
- `cargo build --all-features --all-targets` in test-r — clean.
- `cargo check -p golem-worker-executor-test-utils -p golem-test-framework` — clean.
- `cargo test --all-features -p test-r-core --lib` — 63/63 pass
  (includes the new blanket bridge test).
- `cargo test --all-features -p test-r-example --lib sharing::hosted` — 19/19.
- `cargo test --all-features -p test-r-example-tokio --lib sharing::hosted` — 23/23
  (importantly exercises `LiveAsyncService: AsyncHostedDep` directly,
  confirming the blanket didn't break the non-bridged async path).
- Same under `--spawn-workers --test-threads 4` — 23/23.
- `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings` — clean.
- `cargo fmt --all` — clean.

Oracle review: approved (round 1). Only requested were wording
tightenings (cost claim, conflict wording precision, source-compat
note) — all applied before closure.

#### HR3.2.0 Step 2 — macro front-end: `worker = descriptor | rpc(T) | both(T)` — DONE

Shipped scope (parser and translation only, no runtime work yet):

- Added `pub(crate) enum WorkerView { Descriptor, Rpc(Path), Both(Path) }`
  in `test-r-macro/src/deps.rs` with a hand-rolled
  `impl darling::FromMeta for WorkerView` so we can accept the call
  forms `worker = rpc(Trait)` / `worker = both(Trait)` alongside the
  bare ident `worker = descriptor`. `darling::FromMeta` does not
  natively handle the `ident(args)` shape, hence the custom impl.
- Added `WorkerView::stub_path_from_trait(trait_path) -> Path` that
  appends `Stub` to the last segment of the trait path, preserving
  any qualifier (`some::module::Trait` ⇒ `some::module::TraitStub`).
- Extended `TestDepArgs` with `worker: Option<WorkerView>`.
- Wired the front-end translation in `test_dep()` so:
  - `scope = Hosted` (no `worker`) — descriptor-only, unchanged.
  - `scope = Hosted, worker = descriptor` — accepted, no-op.
  - `scope = Hosted, worker = rpc(Trait)` — internally lowered to
    the existing `Scope::HostedRpc` / `<Trait>Stub` representation
    so the runtime path is unchanged for Step 2.
  - `scope = Hosted, worker = both(Trait)` — currently rejected at
    macro time with an explicit "planned in HR3.2.0 Step 4" message
    that echoes the trait path; it will be implemented end-to-end
    in Step 4.
  - `worker = …` on any non-`Hosted` scope — rejected with a clear
    diagnostic ("`worker = ...` is only valid with `scope = Hosted`").
  - Legacy `scope = HostedRpc, stub = X` — still accepted as before
    (deprecation diagnostic will land alongside Step 4/5 docs).
- The old blanket "`worker = ...` is reserved for HR3.2.0" rejection
  was removed.

Added unit tests in `test-r-macro/src/deps.rs` (`worker_view_tests`
module, 10 tests, all green):

- `parses_descriptor_identifier` — `worker = descriptor` ⇒ `Descriptor`.
- `parses_rpc_with_simple_trait_path` — `worker = rpc(MyTrait)` ⇒
  `Rpc(MyTrait)`.
- `parses_both_with_qualified_trait_path` —
  `worker = both(some::module::Trait)` ⇒ qualifier preserved.
- `rejects_unknown_bare_identifier` — `worker = foo` rejected.
- `rejects_rpc_without_argument` — `worker = rpc()` rejected with
  the "exactly one trait path" diagnostic.
- `rejects_rpc_with_multiple_arguments` — `worker = rpc(A, B)`
  rejected with the same diagnostic.
- `rejects_unknown_call_form` — `worker = wat(Trait)` rejected.
- `rejects_rpc_with_non_path_argument` — `worker = rpc("foo")`
  rejected with the "trait path" diagnostic.
- `stub_path_from_simple_trait` — `MyTrait` ⇒ `MyTraitStub`.
- `stub_path_from_qualified_trait_preserves_qualifier` —
  `some::module::Trait` ⇒ `some::module::TraitStub`.

End-to-end behaviour for the macro translation (a) keeps existing
Hosted / HostedRpc semantics unchanged, and (b) is exercised by the
existing example crates' hosted/hosted_rpc tests. The dedicated
`worker = rpc(T)` / `worker = both(T)` example/integration tests
land with Step 5 alongside the docs update.

Verification (all green):

- `cargo build --all-features --all-targets` in test-r — clean
  (only the pre-existing future-incompat warning from
  `test-r-example-tokio` remains).
- `cargo test --all-features -p test-r-macro --lib worker_view_tests`
  — 10/10 new tests pass.
- `cargo test --all-features -p test-r-macro --lib` — 33/33.
- `cargo test --all-features -p test-r-core --lib` — 63/63.
- `cargo test --all-features -p test-r-example --lib sharing::hosted`
  — 19/19.
- `cargo test --all-features -p test-r-example-tokio --lib sharing::hosted`
  — 23/23.
- `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`
  — clean.
- `cargo fmt --all -- --check` — clean.

Oracle review (round 1): approved with four hardening items, all of
which were applied in-band as part of Step 2:

- `WorkerView::from_meta` now rejects trait paths that carry generic
  arguments on any segment (`rpc(Trait::<T>)`,
  `rpc(outer::Wrapper::<T>::Trait)`, etc.) with a dedicated
  diagnostic. The derived stub would inherit those generics and point
  at a non-existent `TraitStub::<T>`, so failing fast here gives a
  much cleaner error than letting downstream typeck blame the wrong
  span.
- `WorkerView::stub_path_from_trait` now uses
  `quote::format_ident!("{}Stub", last.ident)` instead of the manual
  `Ident::new(&format!(...), span)`. This mirrors the exact
  ident-building strategy used by `#[hosted_rpc]` in `hosted_rpc.rs`
  so the two derivations stay in lockstep on raw-ident / hygiene
  edge cases.
- The `worker = both(T)` rejection message was reworded to front-load
  the action ("not available yet" + concrete alternatives) rather
  than leading with the internal step plan.
- An explicit early rejection for
  `scope = Hosted, worker = rpc(T), async_worker = true` now runs
  *before* the generic `async_worker only valid with scope = Hosted`
  validation. Without it, the user's `scope = Hosted` got rewritten
  to `HostedRpc` during translation and they would see a confusingly
  wrong diagnostic. The new message explains exactly why the flag
  doesn't apply to the RPC path and how to fix it.

Four additional oracle-suggested tests were added in
`worker_view_tests`:

- `stub_path_from_absolute_path_preserves_leading_colons` —
  `::some::krate::Trait` ⇒ `::some::krate::TraitStub`.
- `rejects_rpc_with_turbofish_generic_trait_path`.
- `rejects_both_with_turbofish_generic_trait_path`.
- `rejects_rpc_with_generic_on_intermediate_path_segment` —
  `outer::Wrapper::<T>::Trait` rejected too.

Test count now 14/14 in `worker_view_tests`, 37/37 in `test-r-macro`
lib tests, all other suites unchanged and green. Clippy + fmt still
clean.

#### HR3.2.0 Step 3 — Hosted descriptor: always-async under tokio; deprecate `async_worker` — DONE

Shipped scope (runtime + macro collapse for the Hosted descriptor
path; legacy `async_worker` flag downgraded to a deprecation
warning):

- Added two `#[doc(hidden)] pub fn` helpers in
  `test-r-core/src/lib.rs`:
  - `__test_r_make_hosted_codec::<T>() -> CloneableCodec`
  - `__test_r_make_hosted_worker_reconstructor::<T>() -> WorkerReconstructor`
- Both are cfg-selected on `test-r-core`'s `tokio` cargo feature:
  - tokio build: trait bound `T: AsyncHostedDep`, returns the
    `WorkerReconstructor::Async` variant calling
    `AsyncHostedDep::from_descriptor(...).await`.
  - sync build: trait bound `T: HostedDep`, returns the
    `WorkerReconstructor::Sync` variant calling
    `HostedDep::from_descriptor(...)`.
- The macro (`test-r-macro/src/deps.rs`) no longer branches on
  `async_worker` to pick a descriptor trait path or a
  reconstructor-variant closure. For `scope = Hosted` it now emits a
  single uniform pair of helper calls:
  ```rust
  Some(test_r::core::__test_r_make_hosted_codec::<DepTy>())
  Some(test_r::core::__test_r_make_hosted_worker_reconstructor::<DepTy>())
  ```
- `async_worker = true` is now:
  - on `scope = Hosted` → ignored at the codegen level; the macro
    injects a `#[deprecated]`-tagged local marker function into the
    generated `register_dep_…` `ctor` body and immediately calls it,
    which lights up the standard rustc deprecation warning at the
    dep's registration site.
  - on any non-Hosted scope → still a hard error (wording updated
    to "has been removed for non-Hosted scopes").
- The HR3.2.0 Step 2 special-case rejection
  `async_worker + worker = rpc(...)` was removed: with
  `async_worker` now ignored on Hosted paths, the user now sees the
  ordinary `async_worker` deprecation warning instead of a misleading
  scope error.
- Doc updates on `AsyncHostedDep` in `test-r-core/src/internal.rs`:
  removed the stale "Opt in with `async_worker`" wording, documented
  the auto-select semantics (tokio = async, sync = sync), and
  explicitly noted async-only `AsyncHostedDep` impls fail to compile
  in sync builds rather than panicking at register time as before.

Example migration in `example-tokio/src/sharing/hosted_async_worker.rs`:
- The fixture's `#[test_dep(scope = Hosted, async_worker)]` is now
  `#[test_dep(scope = Hosted)]`.
- Module-level comment updated to call out that worker
  reconstruction is async purely because the owner implements
  `AsyncHostedDep` directly — no flag, no opt-in.

Tests added in `test-r-core/src/lib.rs` (`hosted_helper_tests`
module):

- `make_hosted_codec_round_trips_descriptor_bytes` — owner →
  descriptor bytes → boxed `Vec<u8>` payload round-trip, identical
  on both feature configs.
- `make_hosted_worker_reconstructor_is_async_under_tokio` (tokio
  cfg) — asserts the helper returns
  `WorkerReconstructor::Async(...)`.
- `make_hosted_worker_reconstructor_is_sync_under_sync_runtime`
  (sync cfg) — asserts the helper returns
  `WorkerReconstructor::Sync(...)`.
- `sync_worker_reconstructor_rebuilds_fixture_from_descriptor` (sync
  cfg) — drives the closure end to end against the descriptor bytes
  produced by the codec, asserts the rebuilt value equals the
  original.

Empirical proof of the deprecation path: before fixing the example,
`cargo build --all-features --all-targets` printed exactly one
deprecation warning pointing at the line and column of the
`#[test_dep(scope = Hosted, async_worker)]` attribute on the
fixture. After dropping `async_worker`, the warning vanished. The
warning text was exactly the configured note:

> ``async_worker`` is deprecated and ignored: ``scope = Hosted`` now
> auto-selects descriptor reconstruction (tokio builds use
> `AsyncHostedDep::from_descriptor`, sync builds use
> `HostedDep::from_descriptor`). Remove ``async_worker``.

Verification (all green):

- `cargo build -p test-r-core --features tokio` — clean.
- `cargo build -p test-r-core --no-default-features` — clean.
- `cargo build --all-features --all-targets` — clean (only the
  pre-existing future-incompat warning on `test-r-example-tokio`).
- `cargo test -p test-r-core --features tokio --lib hosted_helper_tests`
  — 2/2.
- `cargo test -p test-r-core --no-default-features --lib hosted_helper_tests`
  — 3/3.
- `cargo test --all-features -p test-r-core --lib` — 65/65.
- `cargo test --no-default-features -p test-r-core --lib` — 62/62.
- `cargo test --all-features -p test-r-macro --lib` — 37/37.
- `cargo test --all-features -p test-r-example --lib sharing::hosted_basic`
  — 4/4 (sync runtime).
- same with `--spawn-workers --test-threads 4` — 4/4.
- `cargo test --all-features -p test-r-example-tokio --lib sharing::hosted_basic`
  — 4/4.
- same with `--spawn-workers --test-threads 4` — 4/4.
- `cargo test --all-features -p test-r-example-tokio --lib sharing::hosted_async_worker`
  — 4/4 (no `async_worker` flag).
- same with `--spawn-workers --test-threads 4` — 4/4.
- `cargo test --all-features -p test-r-example --lib sharing::hosted_rpc_basic -- --spawn-workers --test-threads 4`
  — 8/8.
- `cargo test --all-features -p test-r-example-tokio --lib sharing::hosted_rpc_basic -- --spawn-workers --test-threads 4`
  — 8/8.
- `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`
  — clean.
- `cargo fmt --all -- --check` — clean.

Oracle review (round 1): approved with three actionable items, all
applied in-band:

1. **Real blocker fixed**: the `async_worker` validation now keys
   off the **user-declared** scope captured *before* Step 2's
   `worker = ...` translation rewrites it. Without this fix,
   `scope = Hosted, worker = rpc(T), async_worker` was hard-erroring
   with the misleading "removed for non-Hosted scopes" message even
   though the user wrote exactly `scope = Hosted`.
2. **Wording made worker-view-neutral**: the deprecation note no
   longer claims "auto-selects descriptor reconstruction" (which is
   only true for plain `Hosted`); it now reads "test-r now selects
   the worker-side behavior automatically from the Hosted
   registration shape (descriptor / RPC / both) and the active
   runtime (sync / tokio)".
3. **`AsyncHostedDep` docs tightened**: the "fails to compile in
   sync builds" claim now specifies that it's the
   `#[test_dep(scope = Hosted)]` *registration* that fails for
   async-only impls, not the impl itself.

Added a regression fixture pinning fix #1:
`example-tokio/src/sharing/hosted_async_worker_rpc_legacy.rs`. It
exercises `#[test_dep(scope = Hosted, worker = rpc(LegacyCounter),
async_worker)]` (wrapped in module-level `#[allow(deprecated)]` to
keep CI quiet) and ships a `#[test]` that round-trips one RPC call
through the resulting stub end to end. The test passes both with
and without `--spawn-workers`, proving the legacy combination is
now accepted at the macro level and wired correctly through the
HostedRpc translation rather than being hard-rejected as before.

Also scrubbed stale `async_worker` mentions in
`example-tokio/src/sharing/hosted_async_worker.rs` (module-level
docs and two inline comments) so the example crate is fully
aligned with the new "no flag required" message.

Final post-fix validation (all green):

- `cargo build --all-features --all-targets` — clean.
- `cargo test --all-features -p test-r-core --lib` — 65/65.
- `cargo test --no-default-features -p test-r-core --lib` — 62/62.
- `cargo test --all-features -p test-r-macro --lib` — 37/37.
- `cargo test --all-features -p test-r-example-tokio --lib sharing::hosted_async_worker_rpc_legacy`
  — 1/1, including `--spawn-workers --test-threads 2`.
- All previously-passing hosted / hosted_rpc tests still 4/4 + 8/8
  on both runtimes, with and without `--spawn-workers`.
- `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`
  — clean.
- `cargo fmt --all -- --check` — clean.

HR3.2.0 Step 3 **shipped** — DONE.

#### HR3.2.0 Step 4 — runtime + macro: `worker = both(T)` end to end — DONE

**What changed**

- `test-r-core/src/internal.rs`: `HostedBothShared` shared cell that
  the macro hands to both registrations; holds cached descriptor
  bytes + `Arc<HostedRpcOwnerCell>` so the two views always observe
  the same parent-side owner.
- `test-r-core/src/lib.rs`: three hidden macro-support helpers,
  cfg-selected on the `tokio` feature so descriptor capture goes
  through `AsyncHostedDep::descriptor` under tokio and
  `HostedDep::descriptor` under sync:
  - `__test_r_make_hosted_both_shared::<T>(owner)`
  - `__test_r_make_hosted_both_codec()`
  - `__test_r_make_hosted_both_rpc_factory::<T>()`
- `test-r/src/lib.rs`: re-exported `HostedBothShared` so the
  macro-generated registration in user crates can name it.
- `test-r-macro/src/deps.rs`:
  - `test_dep()` early-branch intercepts
    `scope = Hosted, worker = both(Trait)` and delegates to a new
    `expand_hosted_both_dep` helper, leaving the existing
    single-registration code paths untouched.
  - `expand_hosted_both_dep` lowers one declared dep into **two**
    `RegisteredDependency` entries (Hosted view under the owner
    type's name, HostedRpc view under the `<Trait>Stub` type's name)
    backed by a single weak-cached `Arc<HostedBothShared>` and emits
    a getter for each view so tests can parameterise on either
    `&Owner` or `&<Trait>Stub`.
  - Step 4 surface restrictions are enforced at macro time with
    explicit panics: sync constructor only, no constructor-side dep
    wiring, no `tagged_as`, no `stub = …`. `async_worker` is parsed
    but only emits the Step 3 deprecation warning.
  - The leftover `WorkerView::Both` arm in the Step 2 translation
    match is now `unreachable!()` (defended against future refactors
    that remove the early branch).
- `test-r-core/src/lib.rs` `hosted_helper_tests`: four new unit
  tests pinning the helpers (`make_hosted_both_shared_captures_…`,
  `make_hosted_both_codec_serializes_…`,
  `make_hosted_both_rpc_factory_extracts_owner_cell`,
  `make_hosted_both_rpc_factory_builds_stub`), covering both
  tokio and sync builds.
- `example/src/sharing/hosted_both_basic.rs` (sync) and
  `example-tokio/src/sharing/hosted_both_basic.rs` (tokio): one
  end-to-end fixture each demonstrating the `EnvBasedTestDependencies`
  shape — TCP echo on the descriptor view + monotonic id allocator on
  the RPC view — and pinning the singleton-property regression
  (`OWNER_CTOR_RUNS == 1` in the parent, `0` in worker subprocesses).
  Both fixtures are registered in their crate's `sharing/mod.rs`.

**Oracle follow-up (applied)**

- `expand_hosted_both_dep` now rejects owner/stub dep-name collisions
  at macro time with an actionable message — catches the case where
  the owner type name and the derived `<Trait>Stub` name lower to
  the same dep id, which would otherwise produce duplicate getters
  and ambiguous registrations.
- `example-tokio/src/sharing/hosted_both_async_descriptor.rs`: new
  regression fixture that implements **only** `AsyncHostedDep`
  (genuine `async fn from_descriptor` with a real `.await` body) so
  the `WorkerReconstructor::Async` path on `worker = both(...)` is
  exercised end to end. Pins a counter that the async body actually
  ran (`async_from_descriptor_was_awaited`), and confirms both
  views still resolve to the same parent-side owner. Validated
  in-process **and** under `--spawn-workers --test-threads 2`.

**Validations**

- `cargo build --all-features --all-targets` — clean.
- `cargo test --all-features -p test-r-core --lib hosted_helper_tests`
  — 6/6 (incl. 4 new Step 4 helpers).
- `cargo test --no-default-features -p test-r-core --lib
  hosted_helper_tests` — 7/7 (incl. 4 new Step 4 helpers).
- `cargo test --all-features -p test-r-core --lib` — 69/69.
- `cargo test --no-default-features -p test-r-core --lib` — 66/66.
- `cargo test --all-features -p test-r-macro --lib` — 37/37.
- `cargo test --all-features -p test-r-example --lib sharing::` —
  35/35 (incl. 6 new `hosted_both_basic`), in-process **and**
  `--spawn-workers --test-threads 2`.
- `cargo test --all-features -p test-r-example-tokio --lib
  sharing::` — 34/34 (incl. 6 new `hosted_both_basic`), in-process
  **and** `--spawn-workers --test-threads 2`.
- `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`
  — clean (only the pre-existing `test-r-example-tokio`
  future-incompat warning remains).
- `cargo fmt --all -- --check` — clean.

HR3.2.0 Step 4 **shipped** — DONE.

#### HR3.2.0 Step 5 — examples / docs / book migration to `worker = rpc(T)` sugar — DONE

What landed:

- **Examples migrated to the new sugar where a trait existed.**
  - `example/src/sharing/hosted_rpc_macro.rs` and the tokio mirror
    `example-tokio/src/sharing/hosted_rpc_macro.rs` now register
    their `#[hosted_rpc] trait Counter` owners via
    `#[test_dep(scope = Hosted, worker = rpc(Counter))]` instead of
    the legacy `#[test_dep(scope = HostedRpc, stub = CounterStub)]`.
    Both files' top-of-file docs were rewritten to explain that the
    new sugar is the preferred form for trait-shaped owners, and to
    cross-reference the hand-written-stub `hosted_rpc_basic.rs`
    siblings.
- **Examples that have no trait surface were intentionally left on
  the legacy form.** `example/src/sharing/hosted_rpc_basic.rs` and
  `example-tokio/src/sharing/hosted_rpc_basic.rs` keep the
  `#[test_dep(scope = HostedRpc, stub = LastUniqueIdStub)]`
  registration. Both files document that this is on purpose: the
  example demonstrates the underlying machinery (hand-written stub,
  custom method indices, raw `desert_rust` framing), and there is no
  trait declaration to plug into `worker = rpc(...)`. The doc
  comments now explicitly steer trait-shaped users to the
  `hosted_rpc_macro.rs` sibling and to the new sugar.
- **Book overhaul of `dependency_sharing.md`.**
  - The "Choosing a strategy" bullet list and the strategy table at
    the top now mention the new `worker = …` picker (`descriptor` /
    `rpc(Trait)` / `both(Trait)`).
  - A new "Worker view: `descriptor`, `rpc(Trait)`, `both(Trait)`"
    subsection was added inside the Hosted section. It is a single
    summary table + bullet list that explains what each `worker = …`
    value means, which owner trait(s) are required for it, and how
    it relates to the legacy `HostedRpc` registration. The
    `async_worker` flag is called out as deprecated in this
    subsection too.
  - The existing "Async worker-side reconstruction (`AsyncHostedDep`)"
    subsection was rewritten: the example no longer carries
    `async_worker`, and a `> **Note —**` callout explains that the
    flag is deprecated and ignored on Hosted now that the runtime
    auto-selects sync vs async worker reconstruction.
  - The "HostedRpc" section gained a prominent callout right after
    its heading that points readers at the
    `#[test_dep(scope = Hosted, worker = rpc(<Trait>))]` form for
    trait-shaped owners while documenting why the legacy
    `scope = HostedRpc, stub = <StubType>` form remains supported
    (hand-written stubs / no-trait owners).
  - The `#[hosted_rpc]` attribute macro section's example was
    updated to use `scope = Hosted, worker = rpc(Counter)`, with a
    short follow-up paragraph that says the legacy form still works
    and links back to the hand-written `HostedRpcDep` example for
    the no-trait case.

Validation:
- `cargo build --all-features --all-targets` — clean (only the
  pre-existing `test-r-example-tokio` future-incompat warning).
- Sync example crate:
  - `cargo test -p test-r-example --all-features --lib sharing::hosted_rpc_basic` — 8/8.
  - `cargo test -p test-r-example --all-features --lib sharing::hosted_rpc_basic -- --spawn-workers --test-threads 2` — 8/8.
  - `cargo test -p test-r-example --all-features --lib sharing::hosted_rpc_macro` — 7/7.
  - `cargo test -p test-r-example --all-features --lib sharing::hosted_rpc_macro -- --spawn-workers --test-threads 2` — 7/7.
  - `cargo test -p test-r-example --all-features --lib sharing::` — 35/35 in-process and `--spawn-workers --test-threads 2`.
- Tokio example crate:
  - `cargo test -p test-r-example-tokio --all-features --lib sharing::hosted_rpc_basic` — 8/8.
  - `cargo test -p test-r-example-tokio --all-features --lib sharing::hosted_rpc_basic -- --spawn-workers --test-threads 2` — 8/8.
  - `cargo test -p test-r-example-tokio --all-features --lib sharing::hosted_rpc_macro` — 7/7.
  - `cargo test -p test-r-example-tokio --all-features --lib sharing::hosted_rpc_macro -- --spawn-workers --test-threads 2` — 7/7.
  - `cargo test -p test-r-example-tokio --all-features --lib sharing::` — 40/40 in-process and `--spawn-workers --test-threads 2`.
- `mdbook build book` — clean (HTML book regenerated).
- `cargo fmt --all -- --check` — clean.
- `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`
  — clean (only the pre-existing `test-r-example-tokio`
  future-incompat warning remains).

HR3.2.0 Step 5 **shipped** — DONE. With Steps 1–5 all done, the
HR3.2.0 enabler is complete: the `worker = descriptor | rpc(T) |
both(T)` macro front-end, the runtime support, the
`#[hosted_rpc]`-driven sugar registration, the auto sync/async worker
reconstruction, the `async_worker` deprecation, and the rewritten
examples + book are all in place.

**Oracle Step 5 review follow-ups (applied).** The oracle reviewed
Step 5 and asked for a small docs polish pass plus an optional module
header cleanup. All applied:
- `book/src/advanced_features/dependency_sharing.md`:
  - Top-of-`## Hosted` paragraph: rewritten to make explicit that the
    example below uses `worker = descriptor` (the default) and to
    cross-link the new "Worker view" subsection. The old sentence
    "Implement the `HostedDep` trait... then annotate the constructor
    with `scope = Hosted`" became too broad after HR3.2.0; the new
    wording qualifies it as the descriptor-view case only.
  - "Hosted restrictions" bullet about owner/worker handle sharing
    the same Rust type now explicitly scopes that invariant to
    `worker = descriptor`, and explains that `worker = rpc(Trait)`
    and `worker = both(Trait)` generate a separate `<Trait>Stub` so
    the invariant does not apply to the RPC side.
  - "Worker view" table `both(Trait)` row: trait requirement column
    now reads "Descriptor side: `HostedDep` **or** `AsyncHostedDep`.
    RPC side: `HostedRpcDep`. Both impls are on the same owner type."
    instead of the previous, narrower "Both `HostedDep` and
    `HostedRpcDep`...". This matches what
    `example-tokio/src/sharing/hosted_both_async_descriptor.rs`
    already proves at the integration-test level.
- `example/src/sharing/mod.rs` and
  `example-tokio/src/sharing/mod.rs`: refreshed the module-level doc
  comments to list every supported scope including the Hosted
  worker-view variants, replacing the older "Phases 1A / 1B / 1C"
  internal phase labels with user-facing terminology.

Validation after the polish pass: `cargo fmt --all -- --check`,
`cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`,
and `mdbook build book` all clean. The
`worker-view-descriptor-rpctrait-bothtrait` anchor used by the new
cross-link in the `## Hosted` paragraph was verified against the
rendered HTML (`book/book/advanced_features/dependency_sharing.html`).

#### HR3.2 follow-up in test-r — async ctor support for `worker = both(T)` — DONE

Background. Step 4 deliberately restricted `worker = both(Trait)` to
**sync** owner constructors so the initial lowering could ignore the
async-cache shape. Moving HR3.2 proper into golem hit this restriction
immediately: `EnvBasedTestDependencies::new(...)` is an async fn (it
opens real Redis / RDB / gRPC clients during construction), so it
cannot be migrated to `worker = both(RedisControl)` without lifting
the sync-only macro check.

What changed:

- `test-r-macro/src/deps.rs::expand_hosted_both_dep`:
  - Dropped the unconditional `panic!` on `ast.sig.asyncness.is_some()`.
  - When the user's constructor is `async fn`, the macro now emits:
    1. An `async fn #acquire_ident()` that uses the same
       `OnceLock<Mutex<Weak<HostedBothShared>>>` cache as the sync
       path, with a fast-path upgrade attempt *before* the user's
       ctor is invoked, and a double-checked upgrade *after* — the
       std mutex is never held across `.await`, so racing acquirers
       can't deadlock. In practice the test-r runtime resolves
       Hosted owners serially during plan building so the race is
       defensive only.
    2. A sibling sync `fn #acquire_sync_ident()` that polls the
       async helper exactly once with a no-op waker. It asserts the
       cache is already populated (which the test-r tokio runner
       guarantees by ordering `collect_hosted_descriptor_bytes_async`
       *before* `collect_hosted_rpc_owner_cells_sync`), so the poll
       always returns `Ready(...)` and never invokes the user's
       async ctor. The fallback `Poll::Pending` branch panics with a
       diagnostic message pointing at the runtime ordering
       invariant.
    3. Asymmetric `DependencyConstructor` registrations: the
       Hosted-view registration uses `Async` and drives the user's
       async ctor through `#acquire_ident().await`; the
       HostedRpc-view registration uses `Sync` and goes through
       `#acquire_sync_ident()`. The runtime's
       `collect_hosted_rpc_owner_cells_into` would otherwise panic
       on an async constructor (see `execution.rs` MVP comment).
  - Sync constructors still emit the original symmetric shape (one
    sync acquire helper, two `DependencyConstructor::Sync` calls);
    nothing in the sync path regressed.
- `example-tokio/src/sharing/hosted_both_async_ctor.rs` (new): five
  tests pinning the async-ctor contract end-to-end —
  - descriptor view sees the marker the async ctor populated,
  - RPC view routes back to the same parent-side owner,
  - RPC view returns monotonic ids,
  - both views share a single parent owner,
  - the async ctor body runs **exactly once in the top-level parent**
    and **never inside an IPC worker subprocess** (the load-bearing
    "shared `HostedBothShared` cache de-duplicates" guarantee).
  The fixture's owner constructor does a `tokio::task::yield_now().await`
  so the future actually suspends at least once; a silent fallback to
  a sync wrapper would be caught by the run-count assertion.
- `example-tokio/src/sharing/mod.rs`: registered the new module.

Validation:
- `cargo build --all-features --all-targets` — clean.
- `cargo test -p test-r-example-tokio --all-features --lib sharing::hosted_both_async_ctor`
  in-process and `--spawn-workers --test-threads 2` — 5/5 both modes.
- `cargo test -p test-r-example-tokio --all-features --lib sharing::`
  in-process and `--spawn-workers --test-threads 2` — 45/45 both
  modes (was 40 before — the new async-ctor fixture added five).
- `cargo test -p test-r-example --all-features --lib sharing::` — 35/35.
- `cargo test -p test-r-core --all-features --lib` — 69/69.
- `cargo test -p test-r-macro --all-features --lib` — 37/37.
- `cargo fmt --all` and
  `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`
  — clean (only the pre-existing `test-r-example-tokio`
  future-incompat warning).

#### HR3.2 proper — golem `EnvBasedTestDependencies` migration — DONE

What landed (in `/Users/vigoo/projects/golem/golem`):

- `golem-test-framework/src/config/env.rs`:
  - New `#[test_r::hosted_rpc] pub trait RedisControl` declaration
    with three methods scoped to the parent-held Redis instance:
    - `is_redis_healthy(&self) -> bool` — parent-side equivalent of
      `Redis::assert_valid`, converted to a boolean so workers
      `assert!(...)` on the answer rather than absorb a panic across
      IPC.
    - `flush_redis_db(&self, db: u16) -> Result<(), String>` —
      `FLUSHDB` on the parent-owned `redis::Client`.
    - `redis_prefix(&self) -> String` — owner-side prefix lookup
      used to cross-check the descriptor view from inside workers.
  - `impl RedisControl for EnvBasedTestDependencies` backed by the
    existing `self.redis` `Arc<dyn Redis>`.
  - `impl test_r::core::HostedRpcDep for EnvBasedTestDependencies`
    wiring `RedisControlDispatch::dispatch_redis_control` and
    `RedisControlStub::new` into the standard owner-cell shape, so
    the same owner serves both views.
- `golem-test-framework/src/config/mod.rs`: re-exported
  `RedisControl` and `RedisControlStub` from `env`.
- `integration-tests/tests/agent_config_live_mutation.rs`:
  - Replaced `#[test_dep(scope = Hosted, async_worker)]` with
    `#[test_dep(scope = Hosted, worker = both(RedisControl))]`. The
    `EnvBasedTestDependencies::new(...).await` constructor body is
    unchanged.
  - Refreshed the existing comment to explain the new picker and to
    cross-reference the `RedisControl` trait declaration in
    `golem-test-framework/src/config/env.rs`.
  - New smoke test `redis_control_round_trip` parameterised on both
    `&EnvBasedTestDependencies` and `&RedisControlStub`. It asserts:
    1. `RedisControlStub::is_redis_healthy()` returns `true` (Redis
       reachable from a worker via the RPC surface).
    2. The descriptor-view prefix and the RPC-view prefix agree —
       the cross-view consistency check the `worker = both(T)`
       shape exists for.
    3. `flush_redis_db(15)` on the parent-owned Redis succeeds (a
       deliberately high db index to avoid colliding with concurrent
       traffic from the heavier agent-config suites in the same
       binary).

Validation:
- `cargo build -p golem-test-framework` — clean.
- `cargo build -p integration-tests --tests` — clean; the
  agent-config-live-mutation test binary now compiles against the new
  `worker = both(RedisControl)` registration.
- `cargo build -p golem-test-framework -p integration-tests -p golem-worker-executor -p golem-debugging-service`
  — clean (the broader golem packages that depend on
  `golem-test-framework` still build).
- `cargo test -p golem-test-framework --lib` — 16/16 (the framework
  unit tests, including the existing descriptor serde round-trip
  tests, still pass).
- `cargo fmt -p golem-test-framework -p integration-tests --check`
  — clean.
- `cargo clippy -p golem-test-framework -p integration-tests --tests --no-deps -- -Dwarnings`
  — clean.
- The live `redis_control_round_trip` end-to-end pass requires a
  running Redis (the rest of the `agent-config-live-mutation`
  binary already does), so it runs in CI alongside the other
  agent-config tests; no additional infrastructure required.

#### HR3.2 proper — oracle-review follow-ups — DONE

Oracle review of the test-r-side async `worker = both(...)` lowering
flagged a genuine bug in the pruner: `prune_unused_deps` only walks
real constructor edges (`RegisteredDependency.dependencies`), so a
suite whose selected tests parameterise **only** on the
`&<Trait>Stub` view would have the Hosted owner sibling pruned. For
the async-ctor flavour that empties the shared `HostedBothShared`
cache before the sync HostedRpc-view resolver runs, and the resolver
would then panic with the `Poll::Pending` diagnostic. Tokio runner
ordering by itself is **not** enough.

Fix (`test-r-core` + `test-r-macro` + `test-r`):

- `test-r-core/src/internal.rs`: added a new
  `companions: Vec<String>` field on `RegisteredDependency`. Unlike
  `dependencies`, companions are **planner-only sibling links** —
  no constructor argument is derived from them, no topological
  ordering is implied. The pruner just treats them as mutually
  reachable: if any companion in a group is in the keep-set, the
  whole group is retained.
- `test-r-core/src/execution.rs::prune_unused_deps`: the
  fix-point traversal now expands the keep-set across `companions`
  in addition to the existing `dependencies` walk.
- `test-r/src/lib.rs`: added a new public helper
  `register_dependency_constructor_with_scope_and_companions(..., companions: Vec<String>)`
  alongside the existing `register_dependency_constructor_with_scope`,
  which now delegates to it with an empty companions list. Existing
  call sites (`cargo-test-r`, third-party macro emissions) keep
  compiling unchanged.
- `test-r-macro/src/deps.rs::expand_hosted_both_dep`: the two
  `worker = both(Trait)` registrations (Hosted owner view +
  HostedRpc stub view) now switch to the new helper and declare
  each other as companions. The macro-emitted comment explains why
  the link exists.
- `test-r-macro/src/deps.rs::expand_hosted_both_dep`: tightened the
  `Poll::Pending` diagnostic to call out both possible causes —
  (1) tokio runner Hosted-before-HostedRpc ordering changed, or
  (2) dependency pruning kept only the stub half of a
  `worker = both(...)` registration. With the companion fix in
  place, case (2) is supposed to be unreachable, but the diagnostic
  now points at it for future-proofing.

New regression coverage:

- `test-r-core/src/execution.rs::cloneable_tests::prune_unused_deps_retains_companion_when_only_one_half_is_referenced`:
  pruner-level unit test that pins the contract end-to-end with
  three sub-cases — companion retained from either direction, and a
  control case proving the pruner still drops an unreferenced dep
  that has no companion link.
- `example-tokio/src/sharing/hosted_both_async_ctor_stub_only.rs`
  (new): three async-ctor tests that **only** parameterise on the
  `&<Trait>Stub` view (the case the original fixture didn't
  cover). The stub-only fixture proves end-to-end that the async
  owner constructor still runs exactly once in the parent — and
  never in an IPC worker — even when nothing in the selected test
  set references the `&Owner` view.

Validation after fix:

- `cargo build --all-features --all-targets` — clean.
- `cargo test -p test-r-core --all-features --lib` — 70/70 (was
  69 — pruner-companion regression added).
- `cargo test -p test-r-macro --all-features --lib` — 37/37.
- `cargo test -p test-r-example --all-features --lib sharing::` —
  35/35.
- `cargo test -p test-r-example-tokio --all-features --lib sharing::`
  in-process and `--spawn-workers --test-threads 2` — 48/48 both
  modes (was 45 — three new stub-only-fixture tests added).
- `cargo fmt --all` and
  `cargo clippy --no-deps --all-targets --all-features -- -Dwarnings`
  — clean (only the pre-existing `test-r-example-tokio`
  future-incompat warning).

Strengthened golem smoke test:

- `integration-tests/tests/agent_config_live_mutation.rs::redis_control_round_trip`:
  added a write/flush/read round-trip on Redis db 15 — descriptor
  view writes a sentinel keyed by `std::process::id()`, RPC view
  calls `flush_redis_db(15)`, descriptor view reads back with
  `EXISTS` and asserts the key is gone. This is the stronger
  cross-view consistency check the oracle asked for: if the RPC
  view ever stops routing to the **same** parent-owned Redis
  instance the descriptor view is reading from, the sentinel would
  survive and the test would fail.
- `integration-tests/Cargo.toml`: added a dev-dependency on
  `redis = { workspace = true }` so the smoke test can drive raw
  `redis::cmd("SET")` / `redis::cmd("EXISTS")` calls through the
  descriptor-view connection.

Golem validation after fix:

- `cargo build -p integration-tests --tests` — clean.
- `cargo fmt -p golem-test-framework -p integration-tests --check`
  — clean.
- `cargo clippy -p golem-test-framework -p integration-tests --tests --no-deps -- -Dwarnings`
  — clean.

HR3.2 proper **shipped** — DONE. With Steps 1–6 all done,
`EnvBasedTestDependencies` is now a `worker = both(RedisControl)` dep:
existing bulk-data gRPC paths keep going through the descriptor view
unchanged, and the new RPC surface gives tests a typed way to drive
the parent-held Redis instance from any worker.

HR3.3's golem follow-up has also now landed locally: the integration
fixture root uses `worker = both(WorkerExecutorClusterControl)`, the
cluster/shard-manager lifecycle call sites in `plugins` and
`sharding` route through the generated stub, and no explicit
`scope = Shared` remains under `integration-tests/tests`. See the
"HR3.3 — golem cluster control" section below for the verification
status and the remaining strict full-suite / CI timing caveats.

Historical HR3.2 plan, now completed: pick one container-managing
surface in `EnvBasedTestDependencies` (Redis liveness / kill / flush
was the natural first), define a `#[test_r::hosted_rpc] trait
RedisControl { … }`, and switch `EnvBasedTestDependencies` from
`scope = Hosted, async_worker` to
`scope = Hosted, worker = both(RedisControl)`. No call-site churn for
the bulk-data gRPC paths was needed.

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

### Phase HR3 exit criteria — status

| Criterion | Status |
|-----------|--------|
| HR3.1 `LastUniqueId` lands as first HostedRpc adopter | ✅ DONE (see HR3.1 section) |
| HR3.2 at least one HostedRpc control surface on `EnvBasedTestDependencies` | ✅ DONE (`RedisControl` via `worker = both(RedisControl)`, see HR3.2 proper) |
| HR3.3 `WorkerExecutorTestDependencies` refactor decision | ✅ DONE locally as golem `WorkerExecutorClusterControl` (see "HR3.3 — golem cluster control" below) |
| No test logic changes beyond dep parameter types | ✅ for the HR3.1/HR3.2 migrations; HR3.3 also keeps behavior, except for the parent-side env-override helper needed by the plugin crash-stress restart path. |
| `cargo test` green in CI on the affected golem packages | PARTIAL: golem framework/integration compile + clippy are clean and targeted spawned-worker runs pass; strict full `integration` green and slowest-lane wall-clock remain pending. |

### HR3.3 — golem cluster control (golem follow-up) — DONE locally

HR3.3 was a golem-repo migration, not an upstream `test-r` change: the
upstream HostedRpc primitives, the `worker = both(T)` shape,
async-ctor support, and the pruner-companion fix all shipped in
HR3.2.0 and HR3.2-followup.

What landed locally in `/Users/vigoo/projects/golem/golem`:

1. Added `#[test_r::hosted_rpc] pub trait WorkerExecutorClusterControl`
   in `golem-test-framework/src/config/env.rs`, exposing the cluster
   lifecycle and state-query surface the integration tests need:
   - `kill_all(&self)` / `restart_all(&self)`
   - `restart_all_with_env_vars(&self, vars: Vec<(String, String)>)`
     for parent-side executor restarts with temporary environment
     overrides
   - `stop(&self, idx: u16)` / `start(&self, idx: u16)`
   - `started_indices(&self) -> Vec<u16>` /
     `stopped_indices(&self) -> Vec<u16>`
   - `is_running(&self, idx: u16) -> bool`
   - `cluster_size(&self) -> u16`
   - shard-manager lifecycle helpers (`stop_shard_manager`,
     `start_shard_manager`, `restart_shard_manager`)
   - Redis helper methods repeated from HR3.2 (`is_redis_healthy`,
     `flush_redis_db`, `redis_prefix`) because the current
     `worker = both(T)` shape exposes one RPC trait per owner type.
2. Implemented `WorkerExecutorClusterControl for
   EnvBasedTestDependencies`, forwarding to the existing parent-owned
   worker-executor cluster, shard manager, and Redis handles.
3. Migrated the integration fixture root in
   `integration-tests/tests/lib.rs` to
   `#[test_dep(scope = Hosted, worker = both(WorkerExecutorClusterControl))]`.
4. Migrated `integration-tests/tests/sharding.rs` to the same Hosted +
   RPC shape and routed the worker-executor / shard-manager lifecycle
   helper calls through `&WorkerExecutorClusterControlStub`. The
   sharding suite remains intentionally sequential.
5. Migrated `integration-tests/tests/plugins.rs` crash-stress
   executor restarts through `&WorkerExecutorClusterControlStub`.
6. Converted the remaining explicit `scope = Shared` fixtures under
   `integration-tests/tests` to `PerWorker`; `rg` now finds no
   explicit `scope = Shared` in that tree.

The original HR3.3 checklist mentioned `integration-tests/tests/worker.rs`,
but that file is a module in the `integration` binary, not a separate
binary with its own `create_deps`, and it had no cluster-control call
sites to migrate.

Local verification:

- `cargo check -p golem-test-framework -p integration-tests --tests`
  — clean.
- `cargo clippy -p golem-test-framework -p integration-tests --tests --no-deps -- -Dwarnings`
  — clean.
- `cargo-test-r run --package integration-tests --test integration :tag:group10 -- --spawn-workers --test-threads=2 --report-time`
  — passed. No Shared-dependency fallback warning was printed, and
  the `Running test` / `Finished test` log ordering showed real test
  overlap under two spawned workers.
- `cargo-test-r run --package integration-tests --test integration oplog_processor_crash_stress -- --spawn-workers --test-threads=2 --report-time`
  — passed after adding the parent-side `restart_all_with_env_vars`
  helper.
- Full `cargo-test-r run --package integration-tests --test integration -- --spawn-workers --test-threads=2 --report-time`
  no longer falls back to one thread and shows overlapping execution,
  but is not claimed green: `integration::worker::get_running_workers`
  timed out, and one full run also saw
  `integration::otlp_plugin::otlp_basic_trace_export` time out waiting
  for traces. The OTLP test passed isolated; `get_running_workers`
  also times out isolated in this mode.

Remaining Phase 3.6 closure work after HR3.3:

- Fix or explicitly quarantine the two unrelated full-suite failures
  above, then re-run the full integration binary under capture-on
  spawned workers with `--test-threads=2`.
- Record before/after wall-clock on the slowest golem CI lane and
  document it here.

---

### HR1 deferred items — closure

The three items deferred at HR1.3 closure are tracked here with
final status and rationale.

| Item | Status | Rationale / when to revisit |
|------|--------|-----------------------------|
| Native `async fn` trait methods on the stub | Closed — won't ship unless triggered | The macro rejects async methods at expansion time today; the enclosing `#[test] async fn` can still hold a stub because the tokio runner bridges sync calls via `block_in_place`. Every adopter (test-r examples, golem HR3.1/HR3.2) ships under this model with no friction. Real native async would need either (a) `oneshot`-keyed concurrent in-flight RPCs (the multiplexer below) and a `Future`-returning stub method, or (b) GAT-based ad-hoc futures. Both are non-trivial and not justified by demonstrated adopter need today. Re-open if and only if an adopter ships an `async fn` trait surface they cannot reasonably refactor to sync. |
| `&dyn Trait` / `Arc<dyn Trait>` test parameters | Closed — won't ship unless triggered | The `#[hosted_rpc]` macro generates a concrete `<Trait>Stub` and tests parameterise on `&<Trait>Stub`. This works for every shipped adopter (HR3.1 `LastUniqueId`, HR3.2 `RedisControl`, the example crates) without object-safety friction. Real dyn-trait injection would require test-r's dependency-resolver to know about trait objects (today it keys on concrete type paths) and a separate dyn-safe adapter trait for any future async support. Both are substantial design work for a non-urgent UX polish item. Re-open if a future adopter has multiple owner types implementing the same trait and needs polymorphism at the test parameter. |
| Worker-side reader-task / waiter-table multiplexer | Closed — won't ship unless triggered | The MVP transport (`IpcHostedRpcTransport` in `sync.rs` / `tokio.rs`) holds the per-worker IPC mutex for the full request/response round-trip. The "MVP temporal invariant" — stub calls only happen from inside a running test body — is codified in `HostedRpcDep::build_stub` / `HostedRpcChannel::call` rustdoc and in the book chapter, so the worker's main IPC loop never races with a pending RPC. HR1.0 added explicit regression coverage for >64 KiB payloads, 128 concurrent in-flight calls via `std::thread`, and tokio's `tokio::join!` — all serialise correctly through the mutex without deadlock. A real reader-task / waiter-table multiplexer would be XL work (worker reader loop + waiter table + cancellation / disconnect semantics + interleaving fairness tests + sync/tokio transport integration); the `request_id` field is already on the wire so no protocol change is needed when/if the design is revisited. Re-open if a future adopter needs concurrent in-flight RPCs from *outside* a single test body (e.g., from a detached background tokio task that outlives the test). |

The corresponding "HostedRpc — risks and open questions" rows below
were the original aspirational sketches; the closures above are the
authoritative current state.

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

## Open questions — settled

The four 1A.1 open questions are all resolved by shipped work:

1. **Auto-derive helper for `Wire = Self` where `T: Serde`?** — Decided
   "won't ship". Hand-written `CloneableDep` impls (4–6 lines) have
   been the standard pattern across all Cloneable adopters (golem's 40+
   `compiled_*` deps, wasm-rquickjs Phase 2, the test-r examples) and
   nobody has asked for a derive. A derive would also force a choice
   of serde flavour (serde / desert / bincode) onto every user. Keep
   the explicit trait impl as the supported surface; revisit only if
   adopter feedback ever requests it.

2. **How does `worker_index()` get into worker constructors?** —
   Resolved in Phase 3.3: shipped as a free function
   `test_r::worker_index() -> usize` backed by an `OnceLock<usize>`
   set by the runner from the `--worker-index` CLI flag. No special
   parameter type, no thread-local. Defaults to `0` in the parent and
   in any no-spawn-workers path. Documented in
   `book/src/advanced_features/dependency_sharing.md`.

3. **Should `Shared` deps in a Hosted suite live in the dep host or
   stay in the parent?** — Resolved in Phase 1B by the **parent-hosted**
   redesign: there is no separate dep-host process. The parent
   materialises every Hosted owner once and ships descriptor bytes to
   each worker over the existing IPC socket. The original "sidecar
   dep-host" plan was dropped in favour of this simpler lifetime
   story. Every `Shared` dep therefore lives in the parent today,
   alongside Hosted owners.

4. **Cancellation propagation: if a worker crashes mid-test, do other
   workers continue?** — Confirmed unchanged: yes, other workers
   continue. The Hosted owner cells in the parent are unaffected by
   any single worker's death; the parent's IPC loop fails the affected
   test (or, for HostedRpc traffic, surfaces a
   `HostedRpcError::Transport`) and the remaining workers proceed.
   No suite-wide cancellation flag was added — adopters that want
   abort-on-first-failure can layer one on at the CLI / runner level
   later if a demonstrated need appears.
