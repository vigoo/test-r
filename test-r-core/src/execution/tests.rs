use super::*;
use crate::internal::{
    CloneableCodec, DependencyConstructor, RegisteredDependency, RegisteredTest,
    RegisteredTestSuiteProperty, TestFunction, TestProperties,
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn registered_test(name: &str, deps: Vec<String>) -> RegisteredTest {
    registered_test_in_module(name, "", deps)
}

fn registered_test_in_module(name: &str, module_path: &str, deps: Vec<String>) -> RegisteredTest {
    RegisteredTest {
        name: name.to_string(),
        crate_name: "tcrate".to_string(),
        module_path: module_path.to_string(),
        run: TestFunction::Sync(Arc::new(|_| Box::new(()))),
        props: TestProperties::default(),
        dependencies: Some(deps),
    }
}

/// A Cloneable dep whose constructor increments a counter (so we can
/// assert it ran exactly once), encodes via simple little-endian bytes.
fn registered_cloneable_dep(name: &str, counter: Arc<AtomicUsize>) -> RegisteredDependency {
    registered_cloneable_dep_in(name, "", 0xdead_beef, counter)
}

/// Like [`registered_cloneable_dep`] but lets the caller pick the
/// dep's module path and the constant the constructor emits — so a
/// collision test can assert two same-named deps in different modules
/// don't get crossed up.
fn registered_cloneable_dep_in(
    name: &str,
    module_path: &str,
    constructor_value: u64,
    counter: Arc<AtomicUsize>,
) -> RegisteredDependency {
    let constructor_counter = counter.clone();
    let constructor = DependencyConstructor::Sync(Arc::new(move |_view| {
        constructor_counter.fetch_add(1, Ordering::SeqCst);
        Arc::new(constructor_value) as Arc<dyn Any + Send + Sync>
    }));
    let codec = CloneableCodec {
        to_wire: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            let value: Arc<u64> = any.downcast::<u64>().unwrap();
            (*value).to_le_bytes().to_vec()
        }),
        from_wire_bytes: Arc::new(|bytes: &[u8]| {
            let arr: [u8; 8] = bytes.try_into().unwrap();
            let value = u64::from_le_bytes(arr);
            Arc::new(value) as Arc<dyn Any + Send + Sync>
        }),
    };
    RegisteredDependency {
        name: name.to_string(),
        crate_name: "tcrate".to_string(),
        module_path: module_path.to_string(),
        constructor,
        dependencies: Vec::new(),
        scope: DepScope::Cloneable,
        worker_fn: Some(crate::internal::WorkerReconstructor::Sync(Arc::new(
            |wire_payload, _deps| wire_payload,
        ))),
        cloneable_codec: Some(codec),
        hosted_codec: None,
        rpc_factory: None,
        companions: Vec::new(),
    }
}

/// A `PerWorker` dep that increments `counter` every time its
/// constructor runs. Used by the lifecycle regression tests below to
/// observe whether the runtime accidentally rematerialises a
/// parent-scope dep after `drop_deps` while descendant tests still
/// need it.
fn registered_perworker_counting_dep(
    name: &str,
    module_path: &str,
    counter: Arc<AtomicUsize>,
) -> RegisteredDependency {
    let constructor_counter = counter.clone();
    let constructor = DependencyConstructor::Sync(Arc::new(move |_view| {
        constructor_counter.fetch_add(1, Ordering::SeqCst);
        Arc::new(0u64) as Arc<dyn Any + Send + Sync>
    }));
    RegisteredDependency {
        name: name.to_string(),
        crate_name: "tcrate".to_string(),
        module_path: module_path.to_string(),
        constructor,
        dependencies: Vec::new(),
        scope: DepScope::PerWorker,
        worker_fn: None,
        cloneable_codec: None,
        hosted_codec: None,
        rpc_factory: None,
        companions: Vec::new(),
    }
}

/// Regression test for the
/// "`PerWorker` test-dep constructor called more than once per
/// process" bug.
///
/// The bug lived in `pick_next_internal_sync`: it released a node's
/// materialised deps whenever the round produced no test, even if a
/// descendant subtree still had tests that were only **temporarily
/// unpickable** because another worker thread held a sequential lock.
/// The next pick then rematerialised those deps, re-invoking the
/// user constructor. That re-invocation is fatal for process-global
/// one-shot constructors such as
/// `tracing_subscriber::SubscriberInitExt::init`.
///
/// The async path (`pick_next_internal`) already guarded with
/// `self.is_empty()`; the sync path now mirrors that guard.
///
/// This test reproduces the race deterministically by:
///   1. picking the first test of a sequential child subtree
///      (the returned `TestExecution` holds the child's sequential
///      lock for as long as it stays alive);
///   2. calling `pick_next_sync` again **while still holding** the
///      first `TestExecution` — under the previous behaviour the
///      parent's `PerWorker` dep would be released here, even though
///      the child still has a queued test;
///   3. dropping the first `TestExecution` and picking the second
///      test, and asserting that the constructor ran exactly once
///      across the whole sequence.
#[test]
fn perworker_dep_not_rematerialised_when_descendant_subtree_is_locked() {
    let counter = Arc::new(AtomicUsize::new(0));
    // PerWorker dep registered at the parent module — exactly the
    // shape an `inherit_test_dep!`-d singleton produces (one
    // registration shared by tests in nested sibling modules).
    let dep = registered_perworker_counting_dep("perworker_dep", "parent", counter.clone());
    let test_a =
        registered_test_in_module("t_a", "parent::child", vec!["perworker_dep".to_string()]);
    let test_b =
        registered_test_in_module("t_b", "parent::child", vec!["perworker_dep".to_string()]);
    let sequential_prop = RegisteredTestSuiteProperty::Sequential {
        name: "child".to_string(),
        crate_name: "tcrate".to_string(),
        module_path: "parent".to_string(),
    };

    let (mut execution, _filtered) = TestSuiteExecution::construct(
        &Arguments::default(),
        &[dep],
        &[test_a, test_b],
        &[sequential_prop],
    );

    // Step 1: pick the first test. The returned `TestExecution`
    // owns the `parent::child` sequential lock; keeping it alive
    // models the "another worker thread is still running this test"
    // state during a concurrent `pick_next_sync` call.
    let first = execution
        .pick_next_sync()
        .expect("first test should be picked");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "PerWorker constructor must run once when the parent subtree first materialises"
    );

    // Step 2: try to pick again **before** dropping `first`.
    // Internally `parent::child` is locked, so `pick_next_sync`
    // returns `None`. Under the bug, this is where `parent` would
    // release its materialised deps.
    let none = execution.pick_next_sync();
    assert!(
        none.is_none(),
        "no test should be picked while the sequential lock is held"
    );

    // Step 3: drop the first test (releases the sequential lock) and
    // pick the queued second test. With the bug, the parent's
    // `PerWorker` dep would be re-materialised here and the counter
    // would bump to 2.
    drop(first);
    let second = execution
        .pick_next_sync()
        .expect("second test should be picked");
    drop(second);

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "PerWorker constructor must remain one per process — a temporarily-locked \
         descendant subtree must not cause the parent's materialised deps to be \
         released and rematerialised"
    );
}

/// Async-path counterpart of
/// [`perworker_dep_not_rematerialised_when_descendant_subtree_is_locked`].
///
/// `pick_next_internal` (the `#[cfg(feature = "tokio")]` sibling of
/// `pick_next_internal_sync`) already had the `self.is_empty()` guard
/// before this fix, so this test is defensive coverage: it locks in the
/// async/sync symmetry so that a future change which drops the guard
/// from either path immediately fails CI.
#[cfg(feature = "tokio")]
#[test]
fn perworker_dep_not_rematerialised_when_descendant_subtree_is_locked_async() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let counter = Arc::new(AtomicUsize::new(0));
        let dep = registered_perworker_counting_dep("perworker_dep", "parent", counter.clone());
        let test_a =
            registered_test_in_module("t_a", "parent::child", vec!["perworker_dep".to_string()]);
        let test_b =
            registered_test_in_module("t_b", "parent::child", vec!["perworker_dep".to_string()]);
        let sequential_prop = RegisteredTestSuiteProperty::Sequential {
            name: "child".to_string(),
            crate_name: "tcrate".to_string(),
            module_path: "parent".to_string(),
        };

        let (mut execution, _filtered) = TestSuiteExecution::construct(
            &Arguments::default(),
            &[dep],
            &[test_a, test_b],
            &[sequential_prop],
        );

        // Step 1: pick first test (holds the child's sequential lock).
        let first = execution
            .pick_next()
            .await
            .expect("first test should be picked");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "PerWorker constructor must run once when the parent subtree first materialises"
        );

        // Step 2: pick again while `first` is still alive — the child
        // subtree is locked, so the call returns `None`. The
        // `is_empty()` guard must prevent the parent from releasing
        // its materialised deps here.
        let none = execution.pick_next().await;
        assert!(
            none.is_none(),
            "no test should be picked while the sequential lock is held"
        );

        // Step 3: release the lock and pick the queued second test —
        // the counter must still be 1.
        drop(first);
        let second = execution
            .pick_next()
            .await
            .expect("second test should be picked");
        drop(second);

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "PerWorker constructor must remain one per process on the async path — \
             keep `pick_next_internal` and `pick_next_internal_sync` symmetrical"
        );
    });
}

#[test]
fn cloneable_wire_collection_runs_constructor_once_and_encodes_value() {
    let counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_cloneable_dep("clone_dep", counter.clone());
    let test = registered_test("t1", vec!["clone_dep".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    let collected = execution.collect_cloneable_wire_bytes_sync();
    assert_eq!(collected.len(), 1, "exactly one cloneable dep expected");
    let (dep_id, wire_bytes) = &collected[0];
    assert_eq!(
        dep_id, "tcrate::clone_dep",
        "wire bytes must be keyed by the fully-qualified id, not the local name"
    );
    assert_eq!(
        wire_bytes.as_slice(),
        &0xdead_beef_u64.to_le_bytes(),
        "expected the codec-encoded value to round-trip via to_wire"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "constructor must have run exactly once when collecting"
    );
}

#[test]
fn prune_unused_deps_retains_companion_when_only_one_half_is_referenced() {
    // Regression for `#[test_dep(scope = Hosted, worker = both(T))]`
    // pruning. That lowering registers two dep entries (Hosted owner
    // view + HostedRpc stub view) for a single logical dep, backed by
    // a shared `Arc<HostedBothShared>` cache; the macro now declares
    // the two halves as `companions` of each other so that the pruner
    // retains the Hosted half even when selected tests only reference
    // the stub half. Without that, the async-ctor flavour would panic
    // (`Poll::Pending`) at runtime because the shared cache stays
    // empty.
    //
    // This test reproduces the pruner-level invariant cheaply via two
    // Cloneable deps. The `keep_local` traversal in
    // `prune_unused_deps` should expand the keep-set across
    // companions in either direction.

    // Case A: reference only `dep_a`; `dep_b` is declared as a
    // companion of `dep_a` and must survive pruning.
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));
    let mut dep_a = registered_cloneable_dep("clone_a", counter_a.clone());
    let mut dep_b = registered_cloneable_dep("clone_b", counter_b.clone());
    dep_a.companions = vec!["clone_b".to_string()];
    dep_b.companions = vec!["clone_a".to_string()];

    let test_a = registered_test("t_uses_a", vec!["clone_a".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep_a, dep_b], &[test_a], &[]);

    let kept: Vec<String> = execution
        .collect_cloneable_dependencies()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(
        kept.contains(&"clone_a".to_string()),
        "directly referenced dep must be retained, kept = {kept:?}"
    );
    assert!(
        kept.contains(&"clone_b".to_string()),
        "companion of a retained dep must also be retained (the planner-only \
         sibling link used by `worker = both(...)`), kept = {kept:?}"
    );

    // Case B (reverse direction): reference only `dep_b`, with the
    // same companion link. `dep_a` must survive pruning.
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));
    let mut dep_a = registered_cloneable_dep("clone_a", counter_a.clone());
    let mut dep_b = registered_cloneable_dep("clone_b", counter_b.clone());
    dep_a.companions = vec!["clone_b".to_string()];
    dep_b.companions = vec!["clone_a".to_string()];

    let test_b = registered_test("t_uses_b", vec!["clone_b".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep_a, dep_b], &[test_b], &[]);

    let kept: Vec<String> = execution
        .collect_cloneable_dependencies()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(
        kept.contains(&"clone_a".to_string()),
        "companion of a stub-referenced dep must be retained, kept = {kept:?}"
    );
    assert!(
        kept.contains(&"clone_b".to_string()),
        "directly referenced dep must be retained, kept = {kept:?}"
    );

    // Sanity: a dep with no companion link and not referenced
    // anywhere is still pruned. (Prevents the test above from
    // accidentally turning into "the pruner never drops anything".)
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));
    let dep_a = registered_cloneable_dep("clone_a", counter_a.clone());
    let dep_b = registered_cloneable_dep("clone_b", counter_b.clone());
    let test_a = registered_test("t_uses_a", vec!["clone_a".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep_a, dep_b], &[test_a], &[]);

    let kept: Vec<String> = execution
        .collect_cloneable_dependencies()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(
        kept.contains(&"clone_a".to_string()),
        "directly referenced dep must be retained, kept = {kept:?}"
    );
    assert!(
        !kept.contains(&"clone_b".to_string()),
        "without a companion link, an unreferenced dep must be pruned; \
         kept = {kept:?}"
    );
}

#[test]
fn provide_cloneable_value_short_circuits_constructor() {
    let counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_cloneable_dep("clone_dep", counter.clone());
    let test = registered_test("t1", vec!["clone_dep".to_string()]);

    let (mut execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    let pre_value: Arc<dyn Any + Send + Sync> = Arc::new(99_u64);
    let applied = execution.provide_cloneable_value("tcrate::clone_dep", pre_value);
    assert!(
        applied,
        "pre-populated value should match the dep's qualified id"
    );

    // Pick the test — materialize_deps_sync must reuse the pre-populated
    // value instead of running the original constructor.
    let next = execution.pick_next_sync().expect("test should be picked");
    assert_eq!(next.test.name, "t1");

    let view = next.deps.get("clone_dep").expect("dep available");
    let value: Arc<u64> = view.downcast::<u64>().unwrap();
    assert_eq!(*value, 99);

    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "constructor must not run when a pre-populated value is supplied"
    );
}

#[test]
fn provided_shared_value_is_a_worker_side_leaf() {
    let provided_counter = Arc::new(AtomicUsize::new(0));
    let parent_only_counter = Arc::new(AtomicUsize::new(0));
    let mut provided_dep = registered_cloneable_dep("clone_dep", provided_counter.clone());
    provided_dep.dependencies = vec!["parent_only_dep".to_string()];
    let parent_only_dep = registered_cloneable_dep("parent_only_dep", parent_only_counter.clone());
    let test = registered_test("t1", vec!["clone_dep".to_string()]);

    let (mut execution, _filtered) = TestSuiteExecution::construct(
        &Arguments::default(),
        &[provided_dep, parent_only_dep],
        &[test],
        &[],
    );

    let pre_value: Arc<dyn Any + Send + Sync> = Arc::new(99_u64);
    let applied = execution.provide_cloneable_value("tcrate::clone_dep", pre_value);
    assert!(applied);

    let next = execution.pick_next_sync().expect("test should be picked");
    let view = next.deps.get("clone_dep").expect("dep available");
    let value: Arc<u64> = view.downcast::<u64>().unwrap();
    assert_eq!(*value, 99);
    assert_eq!(
        provided_counter.load(Ordering::SeqCst),
        0,
        "worker-side provided values must not run their original constructor"
    );
    assert_eq!(
        parent_only_counter.load(Ordering::SeqCst),
        0,
        "constructor dependencies are parent-only once a value arrives from wire bytes or an RPC stub"
    );
}

/// Async-constructor counterpart for the parent-side collector used by the
/// tokio runner. Verifies that a Cloneable owner declared with
/// `async fn` is awaited on the parent and its wire bytes are produced
/// keyed by the dep's qualified id.
#[cfg(feature = "tokio")]
#[test]
fn async_cloneable_wire_collection_awaits_async_constructor() {
    use std::pin::Pin;

    let counter = Arc::new(AtomicUsize::new(0));
    let constructor_counter = counter.clone();

    // Build a RegisteredDependency with an Async constructor that
    // genuinely awaits a future (tokio::task::yield_now) on the parent
    // side, then returns a u64.
    let constructor = DependencyConstructor::Async(Arc::new(move |_view| {
        let counter = constructor_counter.clone();
        Box::pin(async move {
            tokio::task::yield_now().await;
            counter.fetch_add(1, Ordering::SeqCst);
            let value: u64 = 0xdead_beef;
            Arc::new(value) as Arc<dyn Any + Send + Sync>
        }) as Pin<Box<dyn std::future::Future<Output = Arc<dyn Any + Send + Sync>>>>
    }));
    let codec = CloneableCodec {
        to_wire: Arc::new(|any| {
            let v: Arc<u64> = any.downcast::<u64>().unwrap();
            (*v).to_le_bytes().to_vec()
        }),
        from_wire_bytes: Arc::new(|bytes| {
            let arr: [u8; 8] = bytes.try_into().unwrap();
            Arc::new(u64::from_le_bytes(arr)) as Arc<dyn Any + Send + Sync>
        }),
    };
    let dep = RegisteredDependency {
        name: "clone_dep".to_string(),
        crate_name: "tcrate".to_string(),
        module_path: String::new(),
        constructor,
        dependencies: Vec::new(),
        scope: DepScope::Cloneable,
        worker_fn: Some(crate::internal::WorkerReconstructor::Sync(Arc::new(
            |wire_payload, _| wire_payload,
        ))),
        cloneable_codec: Some(codec),
        hosted_codec: None,
        rpc_factory: None,
        companions: Vec::new(),
    };
    let test = registered_test("t1", vec!["clone_dep".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let collected = runtime.block_on(execution.collect_cloneable_wire_bytes_async());

    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].0, "tcrate::clone_dep");
    assert_eq!(collected[0].1.as_slice(), &0xdead_beef_u64.to_le_bytes());
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "async constructor must have run exactly once"
    );
}

/// Regression test: two cloneable deps that share a local `name` but live
/// in different modules must not collide on the wire and must not
/// cross-apply on the worker side.
#[test]
fn cloneable_value_routing_uses_qualified_id_across_modules() {
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));

    // Same local name `clone_dep`, different module paths.
    let dep_a = registered_cloneable_dep_in("clone_dep", "mod_a", 11, counter_a.clone());
    let dep_b = registered_cloneable_dep_in("clone_dep", "mod_b", 22, counter_b.clone());

    // One test per module, each takes "clone_dep" from its own module.
    let test_a = registered_test_in_module("t_a", "mod_a", vec!["clone_dep".to_string()]);
    let test_b = registered_test_in_module("t_b", "mod_b", vec!["clone_dep".to_string()]);

    let (execution, _filtered) = TestSuiteExecution::construct(
        &Arguments::default(),
        &[dep_a, dep_b],
        &[test_a, test_b],
        &[],
    );

    // The wire bytes must carry distinct qualified ids and distinct payloads.
    let mut collected = execution.collect_cloneable_wire_bytes_sync();
    collected.sort_by(|l, r| l.0.cmp(&r.0));
    assert_eq!(collected.len(), 2);
    assert_eq!(collected[0].0, "tcrate::mod_a::clone_dep");
    assert_eq!(collected[1].0, "tcrate::mod_b::clone_dep");
    assert_eq!(collected[0].1.as_slice(), &11_u64.to_le_bytes());
    assert_eq!(collected[1].1.as_slice(), &22_u64.to_le_bytes());

    // Worker-side routing: pre-populating with `mod_a`'s qualified id must
    // apply only to `mod_a`'s dep, and similarly for `mod_b`. If the
    // routing fell back to plain `name`, both nodes would be updated.
    let mut execution_a = execution;
    let applied_a =
        execution_a.provide_cloneable_value("tcrate::mod_a::clone_dep", Arc::new(111_u64));
    assert!(applied_a, "mod_a dep must be reachable by qualified id");
    let applied_b =
        execution_a.provide_cloneable_value("tcrate::mod_b::clone_dep", Arc::new(222_u64));
    assert!(applied_b, "mod_b dep must be reachable by qualified id");

    // An unrelated qualified id must not apply to anything.
    let applied_unknown =
        execution_a.provide_cloneable_value("tcrate::mod_c::clone_dep", Arc::new(333_u64));
    assert!(
        !applied_unknown,
        "unknown qualified id must not be applied anywhere"
    );

    // Pick both tests and confirm the per-module values stayed separate.
    let first = execution_a.pick_next_sync().expect("first test");
    let second = execution_a.pick_next_sync().expect("second test");

    let pairs: Vec<(String, u64)> = [first, second]
        .into_iter()
        .map(|n| {
            let v: Arc<u64> = n
                .deps
                .get("clone_dep")
                .expect("dep available")
                .clone()
                .downcast()
                .unwrap();
            (n.test.name.clone(), *v)
        })
        .collect();

    let val_a = pairs
        .iter()
        .find(|(n, _)| n == "t_a")
        .expect("t_a picked")
        .1;
    let val_b = pairs
        .iter()
        .find(|(n, _)| n == "t_b")
        .expect("t_b picked")
        .1;
    assert_eq!(
        val_a, 111,
        "mod_a test must see mod_a's pre-populated value"
    );
    assert_eq!(
        val_b, 222,
        "mod_b test must see mod_b's pre-populated value"
    );

    // Each per-module constructor ran exactly once — during the parent-side
    // wire-bytes collection above. The worker-side `provide_cloneable_value`
    // calls must NOT have triggered the constructor a second time on either
    // node (otherwise the qualified-id routing is wrong / it cross-applied).
    assert_eq!(
        counter_a.load(Ordering::SeqCst),
        1,
        "mod_a constructor must have run exactly once (during wire collection)"
    );
    assert_eq!(
        counter_b.load(Ordering::SeqCst),
        1,
        "mod_b constructor must have run exactly once (during wire collection)"
    );
}

// -------- Hosted dep tests --------

/// Builds a Hosted RegisteredDependency for tests. Owner value is a u64
/// (`payload`). `descriptor()` is modelled by the codec's `to_wire` as
/// the LE bytes of `payload`, and `from_descriptor` is modelled by the
/// worker_fn which downcasts the bytes and rebuilds a u64 — so we can
/// observe both halves of the Hosted round-trip without depending on
/// the user-facing `HostedDep` trait inside this private test helper.
fn registered_hosted_dep(
    name: &str,
    payload: u64,
    owner_counter: Arc<AtomicUsize>,
) -> RegisteredDependency {
    registered_hosted_dep_in(name, "", payload, owner_counter)
}

/// Like [`registered_hosted_dep`] but lets the caller pick the dep's
/// module path — so a collision test can assert two same-named Hosted
/// deps in different modules don't get crossed up on the wire / in the
/// worker routing.
fn registered_hosted_dep_in(
    name: &str,
    module_path: &str,
    payload: u64,
    owner_counter: Arc<AtomicUsize>,
) -> RegisteredDependency {
    let constructor = DependencyConstructor::Sync(Arc::new(move |_view| {
        owner_counter.fetch_add(1, Ordering::SeqCst);
        Arc::new(payload) as Arc<dyn Any + Send + Sync>
    }));
    let codec = CloneableCodec {
        // descriptor() on the owner: encode the payload as LE bytes
        to_wire: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            let v: Arc<u64> = any.downcast::<u64>().unwrap();
            (*v).to_le_bytes().to_vec()
        }),
        // worker side: box the bytes as Any (worker_fn does
        // from_descriptor on them)
        from_wire_bytes: Arc::new(|bytes: &[u8]| {
            let boxed: Vec<u8> = bytes.to_vec();
            Arc::new(boxed) as Arc<dyn Any + Send + Sync>
        }),
    };
    let worker_fn = crate::internal::WorkerReconstructor::Sync(Arc::new(|wire_payload, _deps| {
        let bytes_arc: Arc<Vec<u8>> = wire_payload.downcast::<Vec<u8>>().unwrap();
        let arr: [u8; 8] = (*bytes_arc).as_slice().try_into().unwrap();
        let value: u64 = u64::from_le_bytes(arr);
        Arc::new(value) as Arc<dyn Any + Send + Sync>
    }));
    RegisteredDependency {
        name: name.to_string(),
        crate_name: "tcrate".to_string(),
        module_path: module_path.to_string(),
        constructor,
        dependencies: Vec::new(),
        scope: DepScope::Hosted,
        worker_fn: Some(worker_fn),
        cloneable_codec: None,
        hosted_codec: Some(codec),
        rpc_factory: None,
        companions: Vec::new(),
    }
}

#[test]
fn hosted_descriptor_collection_runs_owner_once_and_keeps_it_alive() {
    let owner_counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_hosted_dep("hosted_dep", 0xcafe_babe_dead_beef, owner_counter.clone());
    let test = registered_test("t1", vec!["hosted_dep".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    let (descriptors, owners) = execution.collect_hosted_descriptor_bytes_sync();
    assert_eq!(descriptors.len(), 1, "exactly one hosted dep expected");
    assert_eq!(owners.len(), 1, "exactly one hosted owner kept alive");

    let (dep_id, descriptor_bytes) = &descriptors[0];
    assert_eq!(
        dep_id, "tcrate::hosted_dep",
        "descriptor must be keyed by the fully-qualified id"
    );
    assert_eq!(
        descriptor_bytes.as_slice(),
        &0xcafe_babe_dead_beef_u64.to_le_bytes(),
        "expected descriptor bytes to match codec.to_wire of payload"
    );
    assert_eq!(
        owner_counter.load(Ordering::SeqCst),
        1,
        "owner constructor must have run exactly once"
    );

    // The returned owner Arc<dyn Any> must wrap the same payload value
    // — i.e. the parent really is holding the owner alive.
    let held: Arc<u64> = owners[0].clone().downcast::<u64>().unwrap();
    assert_eq!(*held, 0xcafe_babe_dead_beef);
}

#[test]
fn hosted_descriptor_roundtrips_to_worker_value_via_provide_cloneable_value() {
    let owner_counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_hosted_dep("hosted_dep", 0x1234_5678_u64, owner_counter.clone());
    let test = registered_test("t1", vec!["hosted_dep".to_string()]);

    let (mut execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    // Worker-side simulation: pre-populate a reconstructed value (this
    // is what `apply_provided_wire_bytes` does after running the worker_fn
    // against the descriptor bytes).
    let pre_value: Arc<dyn Any + Send + Sync> = Arc::new(0x1234_5678_u64);
    let applied = execution.provide_cloneable_value("tcrate::hosted_dep", pre_value);
    assert!(
        applied,
        "Hosted dep must accept pre-populated values via the same path as Cloneable"
    );

    // Pick the test — the owner constructor must NOT run on the worker
    // side (we provided a value directly).
    let next = execution.pick_next_sync().expect("test should be picked");
    let view = next.deps.get("hosted_dep").expect("dep available");
    let value: Arc<u64> = view.downcast::<u64>().unwrap();
    assert_eq!(*value, 0x1234_5678);
    assert_eq!(
        owner_counter.load(Ordering::SeqCst),
        0,
        "Hosted owner constructor must not run on the worker side"
    );
}

#[test]
fn has_hosted_dependencies_reports_correctly() {
    let dep = registered_hosted_dep("h", 0, Arc::new(AtomicUsize::new(0)));
    let test = registered_test("t1", vec!["h".to_string()]);
    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);
    assert!(execution.has_hosted_dependencies());
    assert!(!execution.has_shared_dependencies());
    assert!(!execution.has_cloneable_dependencies());
}

/// The owner constructor must run EXACTLY once even with multiple workers —
/// descriptors are computed once on the parent and shipped to each worker.
#[test]
fn hosted_owner_runs_exactly_once_even_when_collecting_multiple_times() {
    // The collector is what the parent calls once; we verify the
    // expected invariant: a single collect call invokes the owner once
    // (even if multiple Hosted deps share the same dep id structure).
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));

    // Two distinct Hosted deps in the same module/crate.
    let mut dep_a = registered_hosted_dep("hosted_a", 1, counter_a.clone());
    dep_a.name = "hosted_a".to_string();
    let mut dep_b = registered_hosted_dep("hosted_b", 2, counter_b.clone());
    dep_b.name = "hosted_b".to_string();
    let test = registered_test("t1", vec!["hosted_a".to_string(), "hosted_b".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep_a, dep_b], &[test], &[]);

    let (descriptors, owners) = execution.collect_hosted_descriptor_bytes_sync();
    assert_eq!(descriptors.len(), 2);
    assert_eq!(owners.len(), 2);
    assert_eq!(counter_a.load(Ordering::SeqCst), 1);
    assert_eq!(counter_b.load(Ordering::SeqCst), 1);
}

/// Regression test for qualified-id routing on Hosted deps: two Hosted
/// deps that share a local `name` but live in different modules must
/// not collide on the wire and must not cross-apply on the worker side.
/// This mirrors `cloneable_value_routing_uses_qualified_id_across_modules`
/// to make sure the same hardened routing applies to descriptor bytes.
#[test]
fn hosted_descriptor_routing_uses_qualified_id_across_modules() {
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));

    // Same local name `hosted_dep`, different module paths.
    let dep_a = registered_hosted_dep_in("hosted_dep", "mod_a", 11, counter_a.clone());
    let dep_b = registered_hosted_dep_in("hosted_dep", "mod_b", 22, counter_b.clone());

    let test_a = registered_test_in_module("t_a", "mod_a", vec!["hosted_dep".to_string()]);
    let test_b = registered_test_in_module("t_b", "mod_b", vec!["hosted_dep".to_string()]);

    let (execution, _filtered) = TestSuiteExecution::construct(
        &Arguments::default(),
        &[dep_a, dep_b],
        &[test_a, test_b],
        &[],
    );

    // Descriptor bytes must carry distinct qualified ids and distinct payloads.
    let (mut descriptors, _owners) = execution.collect_hosted_descriptor_bytes_sync();
    descriptors.sort_by(|l, r| l.0.cmp(&r.0));
    assert_eq!(descriptors.len(), 2);
    assert_eq!(descriptors[0].0, "tcrate::mod_a::hosted_dep");
    assert_eq!(descriptors[1].0, "tcrate::mod_b::hosted_dep");
    assert_eq!(descriptors[0].1.as_slice(), &11_u64.to_le_bytes());
    assert_eq!(descriptors[1].1.as_slice(), &22_u64.to_le_bytes());

    // Worker-side routing: pre-populating with `mod_a`'s qualified id
    // must apply only to `mod_a`'s dep, and similarly for `mod_b`.
    // Hosted deps use the same routing pathway as Cloneable, so the same
    // qualified-id-routing guarantee applies here.
    let mut execution = execution;
    let applied_a =
        execution.provide_cloneable_value("tcrate::mod_a::hosted_dep", Arc::new(111_u64));
    assert!(
        applied_a,
        "mod_a hosted dep must be reachable by qualified id"
    );
    let applied_b =
        execution.provide_cloneable_value("tcrate::mod_b::hosted_dep", Arc::new(222_u64));
    assert!(
        applied_b,
        "mod_b hosted dep must be reachable by qualified id"
    );

    let applied_unknown =
        execution.provide_cloneable_value("tcrate::mod_c::hosted_dep", Arc::new(333_u64));
    assert!(
        !applied_unknown,
        "unknown qualified id must not be applied to any dep"
    );

    let first = execution.pick_next_sync().expect("first test");
    let second = execution.pick_next_sync().expect("second test");
    let pairs: Vec<(String, u64)> = [first, second]
        .into_iter()
        .map(|n| {
            let v: Arc<u64> = n
                .deps
                .get("hosted_dep")
                .expect("dep available")
                .clone()
                .downcast()
                .unwrap();
            (n.test.name.clone(), *v)
        })
        .collect();

    let val_a = pairs
        .iter()
        .find(|(n, _)| n == "t_a")
        .expect("t_a picked")
        .1;
    let val_b = pairs
        .iter()
        .find(|(n, _)| n == "t_b")
        .expect("t_b picked")
        .1;
    assert_eq!(val_a, 111);
    assert_eq!(val_b, 222);

    // Each per-module owner constructor must have run exactly once
    // (during the parent-side descriptor collection above); the
    // worker-side provide_cloneable_value calls must not have re-run
    // them on either node.
    assert_eq!(counter_a.load(Ordering::SeqCst), 1);
    assert_eq!(counter_b.load(Ordering::SeqCst), 1);
}

/// Mode-consistency regression test for the Hosted scope: when the
/// runner does NOT spawn workers (e.g. `--nocapture`), tests must
/// still see the *worker-side handle* produced by the registered
/// `worker_fn` (i.e. `HostedDep::from_descriptor`), not the raw owner
/// value returned by the parent constructor. This exercises the same
/// codec + worker_fn round-trip that the runner-side
/// `apply_hosted_descriptors_locally` helpers in sync.rs / tokio.rs
/// perform on the no-spawn-workers path.
#[test]
fn hosted_no_spawn_workers_uses_worker_side_handle() {
    // Build a Hosted dep whose owner is one u64 value but whose
    // worker reconstructor produces a DIFFERENT u64 value. If the
    // local code path goes through descriptor->worker_fn correctly,
    // the test must see the worker value (not the owner value).
    let owner_counter = Arc::new(AtomicUsize::new(0));
    let constructor_counter = owner_counter.clone();
    let owner_value: u64 = 0xAAAA_AAAA_AAAA_AAAA_u64;
    let constructor = DependencyConstructor::Sync(Arc::new(move |_view| {
        constructor_counter.fetch_add(1, Ordering::SeqCst);
        Arc::new(owner_value) as Arc<dyn Any + Send + Sync>
    }));
    // Owner-side codec serialises the owner value as raw LE bytes.
    // The worker side wraps those bytes in `Vec<u8>` and the
    // worker_fn flips every bit to demonstrate the worker reconstruction
    // path is taken (the bit-flip stands in for any non-identity
    // `HostedDep::from_descriptor` implementation).
    let codec = CloneableCodec {
        to_wire: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            let v: Arc<u64> = any.downcast::<u64>().unwrap();
            (*v).to_le_bytes().to_vec()
        }),
        from_wire_bytes: Arc::new(|bytes: &[u8]| {
            let boxed: Vec<u8> = bytes.to_vec();
            Arc::new(boxed) as Arc<dyn Any + Send + Sync>
        }),
    };
    let worker_fn = crate::internal::WorkerReconstructor::Sync(Arc::new(|wire_payload, _deps| {
        let bytes_arc: Arc<Vec<u8>> = wire_payload.downcast::<Vec<u8>>().unwrap();
        let arr: [u8; 8] = (*bytes_arc).as_slice().try_into().unwrap();
        let raw: u64 = u64::from_le_bytes(arr);
        let handle_value: u64 = !raw;
        Arc::new(handle_value) as Arc<dyn Any + Send + Sync>
    }));
    let dep = RegisteredDependency {
        name: "hosted_dep".to_string(),
        crate_name: "tcrate".to_string(),
        module_path: String::new(),
        constructor,
        dependencies: Vec::new(),
        scope: DepScope::Hosted,
        worker_fn: Some(worker_fn.clone()),
        cloneable_codec: None,
        hosted_codec: Some(codec.clone()),
        rpc_factory: None,
        companions: Vec::new(),
    };
    let test = registered_test("t1", vec!["hosted_dep".to_string()]);

    let (mut execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    // Parent runs the owner constructor once and collects descriptor bytes
    // (mirroring `collect_hosted_descriptor_bytes_sync` invoked by the
    // no-spawn-workers parent runner).
    let (descriptors, owners) = execution.collect_hosted_descriptor_bytes_sync();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(owners.len(), 1);
    let (dep_id, wire_bytes) = &descriptors[0];

    // Parent reconstructs the WORKER-side handle locally via
    // codec.from_wire_bytes + worker_fn (mirroring
    // `apply_hosted_descriptors_locally` in sync.rs / tokio.rs).
    let wire_payload = (codec.from_wire_bytes)(wire_bytes.as_slice());
    let empty_deps: Arc<dyn crate::internal::DependencyView + Send + Sync> =
        Arc::new(HashMap::<String, Arc<dyn Any + Send + Sync>>::new());
    let reconstructed = match &worker_fn {
        crate::internal::WorkerReconstructor::Sync(f) => f(wire_payload, empty_deps),
        crate::internal::WorkerReconstructor::Async(_) => unreachable!(),
    };
    let applied = execution.provide_cloneable_value(dep_id, reconstructed);
    assert!(applied);

    // Pick the test — it must see the WORKER handle (~owner), NOT the owner
    // constructor's return value.
    let next = execution.pick_next_sync().expect("test picked");
    let view = next.deps.get("hosted_dep").expect("dep available");
    let value: Arc<u64> = view.clone().downcast::<u64>().unwrap();
    assert_eq!(
        *value,
        !owner_value,
        "Hosted dep must expose the worker-side handle (from_descriptor) even in the no-spawn-workers path"
    );
    assert_eq!(
        owner_counter.load(Ordering::SeqCst),
        1,
        "owner constructor must have run exactly once during descriptor collection"
    );
}

/// Hosted owners construct their dependencies in the parent collection
/// context. Worker-side values reconstructed from descriptors are leaves;
/// the constructor dependencies are not re-created from wire bytes.
#[test]
fn hosted_dep_with_owner_dependencies_constructs_in_parent_context() {
    let dep_counter = Arc::new(AtomicUsize::new(0));
    let owner_counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_cloneable_dep("some_other_dep", dep_counter.clone());
    let mut hosted = registered_hosted_dep("h_with_deps", 0, owner_counter.clone());
    hosted.dependencies = vec!["some_other_dep".to_string()];
    let test = registered_test("t1", vec!["h_with_deps".to_string()]);
    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep, hosted], &[test], &[]);
    let collected = execution.collect_parent_shared_dependencies_sync();

    assert_eq!(collected.hosted_descriptor_bytes.len(), 1);
    assert_eq!(dep_counter.load(Ordering::SeqCst), 1);
    assert_eq!(owner_counter.load(Ordering::SeqCst), 1);
}

/// Tokio path: async owner constructors are awaited on the parent's
/// collector.
#[cfg(feature = "tokio")]
#[test]
fn async_hosted_descriptor_collection_awaits_async_constructor() {
    use std::pin::Pin;

    let counter = Arc::new(AtomicUsize::new(0));
    let constructor_counter = counter.clone();

    let constructor = DependencyConstructor::Async(Arc::new(move |_view| {
        let counter = constructor_counter.clone();
        Box::pin(async move {
            tokio::task::yield_now().await;
            counter.fetch_add(1, Ordering::SeqCst);
            let value: u64 = 42;
            Arc::new(value) as Arc<dyn Any + Send + Sync>
        }) as Pin<Box<dyn std::future::Future<Output = Arc<dyn Any + Send + Sync>>>>
    }));
    let codec = CloneableCodec {
        to_wire: Arc::new(|any| {
            let v: Arc<u64> = any.downcast::<u64>().unwrap();
            (*v).to_le_bytes().to_vec()
        }),
        from_wire_bytes: Arc::new(|bytes| {
            let boxed: Vec<u8> = bytes.to_vec();
            Arc::new(boxed) as Arc<dyn Any + Send + Sync>
        }),
    };
    let dep = RegisteredDependency {
        name: "hosted_async".to_string(),
        crate_name: "tcrate".to_string(),
        module_path: String::new(),
        constructor,
        dependencies: Vec::new(),
        scope: DepScope::Hosted,
        worker_fn: Some(crate::internal::WorkerReconstructor::Sync(Arc::new(
            |wire_payload, _| {
                let bytes_arc: Arc<Vec<u8>> = wire_payload.downcast::<Vec<u8>>().unwrap();
                let arr: [u8; 8] = (*bytes_arc).as_slice().try_into().unwrap();
                let value: u64 = u64::from_le_bytes(arr);
                Arc::new(value) as Arc<dyn Any + Send + Sync>
            },
        ))),
        cloneable_codec: None,
        hosted_codec: Some(codec),
        rpc_factory: None,
        companions: Vec::new(),
    };
    let test = registered_test("t1", vec!["hosted_async".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (descriptors, owners) = runtime.block_on(execution.collect_hosted_descriptor_bytes_async());

    assert_eq!(descriptors.len(), 1);
    assert_eq!(owners.len(), 1);
    assert_eq!(descriptors[0].0, "tcrate::hosted_async");
    assert_eq!(descriptors[0].1.as_slice(), &42_u64.to_le_bytes());
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    let held: Arc<u64> = owners[0].clone().downcast::<u64>().unwrap();
    assert_eq!(*held, 42);
}

// ===========================================================
// HostedRpc unit tests
// ===========================================================

use crate::internal::{
    HostedRpcChannel, HostedRpcDep, HostedRpcError, HostedRpcOwnerCell, HostedRpcTransport,
    InProcessHostedRpcTransport, RpcFactory,
};

/// Owner type for HostedRpc unit tests. A trivial monotonic counter
/// that increments on every `dispatch(method_idx=1, _)` call and
/// returns the new value as big-endian bytes.
struct RpcCounter {
    n: u64,
}

impl HostedRpcDep for RpcCounter {
    type Stub = RpcCounterStub;
    fn dispatch(&mut self, method_idx: u32, args: &[u8]) -> Result<Vec<u8>, String> {
        match method_idx {
            1 => {
                self.n += 1;
                Ok(self.n.to_be_bytes().to_vec())
            }
            // Large-payload echo. Args are a 4-byte big-endian
            // u32 size; the owner returns `size` bytes filled with a
            // deterministic `i % 251` pattern so framing corruption
            // is caught explicitly, not just length mismatch.
            2 => {
                let arr: [u8; 4] = args
                    .try_into()
                    .map_err(|_| "method_idx=2 requires exactly 4 bytes (size)".to_string())?;
                let size = u32::from_be_bytes(arr) as usize;
                let mut out = vec![0u8; size];
                for (i, b) in out.iter_mut().enumerate() {
                    *b = (i % 251) as u8;
                }
                Ok(out)
            }
            other => Err(format!("RpcCounter: unknown method_idx {other}")),
        }
    }
    fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
        RpcCounterStub { channel }
    }
}

/// Worker-visible stub for the test owner above.
struct RpcCounterStub {
    channel: HostedRpcChannel,
}

impl RpcCounterStub {
    fn next(&self) -> u64 {
        let bytes = self.channel.call(1, Vec::new()).expect("rpc call");
        let arr: [u8; 8] = bytes.as_slice().try_into().unwrap();
        u64::from_be_bytes(arr)
    }

    /// Request `size` bytes back from the owner.
    fn echo(&self, size: u32) -> Vec<u8> {
        self.channel
            .call(2, size.to_be_bytes().to_vec())
            .expect("echo rpc call")
    }
}

/// Builds a HostedRpc `RegisteredDependency` for tests. The constructor
/// wraps an [`RpcCounter`] into a [`HostedRpcOwnerCell`] (mirroring the
/// macro-emitted code), counts its own runs in `counter`, and the
/// `RpcFactory` performs the symmetric downcast back to a cell.
fn registered_hosted_rpc_dep(
    name: &str,
    module_path: &str,
    owner_counter: Arc<AtomicUsize>,
) -> RegisteredDependency {
    let ctor_counter = owner_counter.clone();
    let constructor = DependencyConstructor::Sync(Arc::new(move |_view| {
        ctor_counter.fetch_add(1, Ordering::SeqCst);
        let cell = HostedRpcOwnerCell::from_owner(RpcCounter { n: 0 });
        Arc::new(cell) as Arc<dyn Any + Send + Sync>
    }));
    let factory = RpcFactory {
        owner_into_cell: Arc::new(|any: Arc<dyn Any + Send + Sync>| {
            any.downcast::<HostedRpcOwnerCell>()
                .expect("HostedRpc owner downcast")
        }),
        build_stub: Arc::new(|channel: HostedRpcChannel| {
            let stub = <RpcCounter as HostedRpcDep>::build_stub(channel);
            Arc::new(stub) as Arc<dyn Any + Send + Sync>
        }),
    };
    RegisteredDependency {
        name: name.to_string(),
        crate_name: "tcrate".to_string(),
        module_path: module_path.to_string(),
        constructor,
        dependencies: Vec::new(),
        scope: DepScope::HostedRpc,
        worker_fn: None,
        cloneable_codec: None,
        hosted_codec: None,
        rpc_factory: Some(factory),
        companions: Vec::new(),
    }
}

#[test]
fn hosted_rpc_owner_cells_collected_once_and_keyed_by_qualified_id() {
    let counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_hosted_rpc_dep("rpc_dep", "", counter.clone());
    let test = registered_test("t1", vec!["rpc_dep".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    assert!(execution.has_hosted_rpc_dependencies());

    let cells = execution.collect_hosted_rpc_owner_cells_sync();
    assert_eq!(cells.len(), 1, "exactly one hosted rpc dep expected");
    let (dep_id, _cell) = &cells[0];
    assert_eq!(dep_id, "tcrate::rpc_dep");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "owner constructor must run exactly once on the parent"
    );

    // Collecting again must not re-run the constructor in the same
    // execution tree (the constructor is called inside collect, not
    // memoised). This asserts that we don't accidentally double-collect
    // when the runner makes the call.
    let cells_b = execution.collect_hosted_rpc_owner_cells_sync();
    assert_eq!(cells_b.len(), 1);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "collect_hosted_rpc_owner_cells_sync runs the constructor on every call; \
         callers (the runner) are responsible for only calling it once per suite"
    );
}

#[test]
fn hosted_rpc_owner_dependencies_construct_in_parent_context() {
    let parent_only_counter = Arc::new(AtomicUsize::new(0));
    let owner_counter = Arc::new(AtomicUsize::new(0));
    let parent_only_dep = registered_cloneable_dep("parent_only_dep", parent_only_counter.clone());
    let mut rpc_dep = registered_hosted_rpc_dep("rpc_dep", "", owner_counter.clone());
    rpc_dep.dependencies = vec!["parent_only_dep".to_string()];
    let test = registered_test("t1", vec!["rpc_dep".to_string()]);

    let (execution, _filtered) = TestSuiteExecution::construct(
        &Arguments::default(),
        &[parent_only_dep, rpc_dep],
        &[test],
        &[],
    );

    let cells = execution.collect_hosted_rpc_owner_cells_sync();
    assert_eq!(cells.len(), 1);
    assert_eq!(parent_only_counter.load(Ordering::SeqCst), 1);
    assert_eq!(owner_counter.load(Ordering::SeqCst), 1);
}

#[test]
fn hosted_rpc_in_process_transport_routes_to_owner_cell() {
    let counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_hosted_rpc_dep("rpc_dep", "", counter.clone());
    let test = registered_test("t1", vec!["rpc_dep".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    let cells: HashMap<String, Arc<HostedRpcOwnerCell>> = execution
        .collect_hosted_rpc_owner_cells_sync()
        .into_iter()
        .collect();

    let transport: Arc<dyn HostedRpcTransport> =
        Arc::new(InProcessHostedRpcTransport::new(cells.clone()));
    let channel = HostedRpcChannel::new("tcrate::rpc_dep".to_string(), transport.clone());
    let stub = <RpcCounter as HostedRpcDep>::build_stub(channel);

    assert_eq!(stub.next(), 1);
    assert_eq!(stub.next(), 2);
    assert_eq!(stub.next(), 3);
}

#[test]
fn hosted_rpc_in_process_transport_returns_dispatch_error_on_unknown_method() {
    let counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_hosted_rpc_dep("rpc_dep", "", counter);
    let test = registered_test("t1", vec!["rpc_dep".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);

    let cells: HashMap<String, Arc<HostedRpcOwnerCell>> = execution
        .collect_hosted_rpc_owner_cells_sync()
        .into_iter()
        .collect();
    let transport: Arc<dyn HostedRpcTransport> =
        Arc::new(InProcessHostedRpcTransport::new(cells.clone()));
    let channel = HostedRpcChannel::new("tcrate::rpc_dep".to_string(), transport.clone());

    // Call an unknown method index directly; the owner's `dispatch`
    // returns `Err("…")` which the transport surfaces as a Dispatch error.
    let err = channel.call(999, Vec::new()).unwrap_err();
    match err {
        HostedRpcError::Dispatch(msg) => {
            assert!(
                msg.contains("unknown method_idx 999"),
                "expected dispatch error to mention method_idx, got '{msg}'"
            );
        }
        HostedRpcError::Transport(msg) => {
            panic!("expected Dispatch error, got Transport({msg})");
        }
    }
}

#[test]
fn hosted_rpc_in_process_transport_returns_transport_error_on_unknown_dep_id() {
    let cells: HashMap<String, Arc<HostedRpcOwnerCell>> = HashMap::new();
    let transport: Arc<dyn HostedRpcTransport> = Arc::new(InProcessHostedRpcTransport::new(cells));
    let channel = HostedRpcChannel::new("tcrate::missing_dep".to_string(), transport.clone());
    let err = channel.call(1, Vec::new()).unwrap_err();
    match err {
        HostedRpcError::Transport(msg) => {
            assert!(
                msg.contains("unknown dep id 'tcrate::missing_dep'"),
                "expected transport error to mention dep id, got '{msg}'"
            );
        }
        HostedRpcError::Dispatch(msg) => {
            panic!("expected Transport error, got Dispatch({msg})");
        }
    }
}

// -------------------------------------------------------------
// Coverage for owner panic + mutex poisoning. The owner-cell catches the
// panic, turns it into `Err("hosted rpc owner panicked: ...")` for the
// first call, and subsequent calls hit the poisoned mutex and get the
// stable `"hosted rpc owner poisoned"` error.
// -------------------------------------------------------------

/// Owner that panics on every dispatch call. Used to exercise the
/// catch_unwind + poisoned-mutex paths in `HostedRpcOwnerCell::dispatch`.
struct PanickingRpcOwner;

impl HostedRpcDep for PanickingRpcOwner {
    type Stub = RpcCounterStub;
    fn dispatch(&mut self, _method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
        panic!("owner_panic_for_test");
    }
    fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
        RpcCounterStub { channel }
    }
}

/// Async owner used to verify `AsyncHostedRpcDep`-flavoured cells
/// dispatch through the async path and surface results correctly.
/// Yields once to force the future to actually `.await` (a no-op
/// `std::future::ready` wouldn't exercise the async cell machinery).
#[cfg(feature = "tokio")]
struct AsyncRpcCounter {
    n: u64,
}

#[cfg(feature = "tokio")]
impl crate::internal::AsyncHostedRpcDep for AsyncRpcCounter {
    type Stub = RpcCounterStub;
    async fn dispatch(&mut self, method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
        // Force a real `.await` so the async dispatch machinery
        // is actually exercised (not just a `ready(...)` bridge).
        ::tokio::task::yield_now().await;
        if method_idx == 1 {
            self.n += 1;
            Ok(self.n.to_be_bytes().to_vec())
        } else {
            Err(format!("AsyncRpcCounter: unknown method_idx {method_idx}"))
        }
    }
    fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
        RpcCounterStub { channel }
    }
}

/// Async owner that always panics — exercises the
/// `futures::FutureExt::catch_unwind` + poison-flag machinery on
/// the async cell variant.
#[cfg(feature = "tokio")]
struct PanickingAsyncRpcOwner;

#[cfg(feature = "tokio")]
impl crate::internal::AsyncHostedRpcDep for PanickingAsyncRpcOwner {
    type Stub = RpcCounterStub;
    async fn dispatch(&mut self, _method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
        ::tokio::task::yield_now().await;
        panic!("async_owner_panic_for_test");
    }
    fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
        RpcCounterStub { channel }
    }
}

/// End-to-end: an async owner registered via `from_async_owner`
/// dispatches via `dispatch_async` and returns the expected bytes
/// after actually `.await`ing inside its method body.
#[cfg(feature = "tokio")]
#[test]
fn async_hosted_rpc_owner_dispatches_through_async_cell() {
    let cell = HostedRpcOwnerCell::from_async_owner(AsyncRpcCounter { n: 0 });
    let rt = ::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let bytes_a = rt
        .block_on(cell.dispatch_async(1, &[]))
        .expect("first async dispatch must succeed");
    assert_eq!(bytes_a, 1u64.to_be_bytes().to_vec());

    let bytes_b = rt
        .block_on(cell.dispatch_async(1, &[]))
        .expect("second async dispatch must succeed");
    assert_eq!(bytes_b, 2u64.to_be_bytes().to_vec());
}

/// An async owner panic must surface as
/// `"hosted rpc owner panicked: ..."` and poison the cell so every
/// subsequent dispatch short-circuits with the stable
/// `"hosted rpc owner poisoned"` error. Mirrors the sync test.
#[cfg(feature = "tokio")]
#[test]
fn async_hosted_rpc_owner_panic_surfaces_then_poisons() {
    let cell = HostedRpcOwnerCell::from_async_owner(PanickingAsyncRpcOwner);
    let rt = ::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let err1 = rt
        .block_on(cell.dispatch_async(1, &[]))
        .expect_err("first async dispatch must surface the panic as Err");
    assert!(
        err1.contains("hosted rpc owner panicked: async_owner_panic_for_test"),
        "expected first-call error to wrap the async panic payload, got '{err1}'"
    );

    let err2 = rt
        .block_on(cell.dispatch_async(1, &[]))
        .expect_err("second async dispatch must short-circuit on the poisoned cell");
    assert_eq!(
        err2, "hosted rpc owner poisoned",
        "expected poisoned-cell error on the second async call, got '{err2}'"
    );
}

/// Regression for the async poison race: a second dispatch that
/// parks on `tokio::sync::Mutex::lock().await` *before* the first
/// dispatch panics must still observe the poison flag once it
/// acquires the mutex, and must not re-enter the owner. Without
/// the in-lock re-check the second waiter would get a fresh
/// `MutexGuard` and call the user method on the half-mutated
/// owner.
#[cfg(feature = "tokio")]
#[test]
fn async_hosted_rpc_owner_poison_blocks_concurrent_waiter() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Counts dispatch entries to verify the second call never
    /// re-enters after the first call's panic.
    struct OnePanicThenForbidden {
        entries: Arc<AtomicUsize>,
    }

    impl crate::internal::AsyncHostedRpcDep for OnePanicThenForbidden {
        type Stub = RpcCounterStub;
        async fn dispatch(&mut self, _method_idx: u32, _args: &[u8]) -> Result<Vec<u8>, String> {
            let n = self.entries.fetch_add(1, Ordering::SeqCst);
            // First entry: hold the mutex for long enough that a
            // second dispatch parks on `lock().await`, then panic.
            if n == 0 {
                ::tokio::time::sleep(Duration::from_millis(50)).await;
                panic!("first_dispatch_panic_poison_race");
            }
            // Any subsequent re-entry is a bug: the poison flag
            // re-check inside the lock should have short-circuited
            // before we got here.
            panic!("second_dispatch_unexpectedly_re_entered_after_poison");
        }
        fn build_stub(channel: HostedRpcChannel) -> Self::Stub {
            RpcCounterStub { channel }
        }
    }

    let entries = Arc::new(AtomicUsize::new(0));
    let cell = Arc::new(HostedRpcOwnerCell::from_async_owner(
        OnePanicThenForbidden {
            entries: entries.clone(),
        },
    ));

    let rt = ::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    rt.block_on(async {
        let cell_a = cell.clone();
        let cell_b = cell.clone();

        let first = ::tokio::spawn(async move { cell_a.dispatch_async(1, &[]).await });

        // Give `first` a head start so it definitely owns the
        // mutex before `second` starts and parks on `lock().await`.
        ::tokio::time::sleep(Duration::from_millis(5)).await;

        let second = ::tokio::spawn(async move { cell_b.dispatch_async(1, &[]).await });

        let first_res = first.await.expect("first task must not be cancelled");
        let second_res = second.await.expect("second task must not be cancelled");

        let first_err =
            first_res.expect_err("first dispatch must surface the panic as Err, not Ok");
        assert!(
            first_err.contains("hosted rpc owner panicked: first_dispatch_panic_poison_race"),
            "expected the first call to surface the panic; got '{first_err}'"
        );

        let second_err = second_res
            .expect_err("second dispatch must short-circuit on the poisoned cell, not Ok");
        assert_eq!(
            second_err, "hosted rpc owner poisoned",
            "expected the second waiter to see the poison flag; got '{second_err}'"
        );
    });

    // Exactly one entry into the owner: the second waiter must
    // have been turned away by the poison re-check, never reaching
    // the user dispatcher body.
    assert_eq!(
        entries.load(Ordering::SeqCst),
        1,
        "owner dispatcher must run at most once across the poisoned pair"
    );
}

/// `dispatch_blocking` against an `Async` cell on a multi-thread
/// tokio runtime must succeed (it bridges to `dispatch_async`
/// via `block_in_place` + `block_on`).
#[cfg(feature = "tokio")]
#[test]
fn async_hosted_rpc_dispatch_blocking_drives_async_cell() {
    let cell = HostedRpcOwnerCell::from_async_owner(AsyncRpcCounter { n: 0 });
    let rt = ::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let bytes = rt
        .block_on(async {
            ::tokio::task::spawn_blocking(move || cell.dispatch_blocking(1, &[])).await
        })
        .expect("spawn_blocking joined")
        .expect("dispatch_blocking must succeed against an async cell on multi-thread rt");
    assert_eq!(bytes, 1u64.to_be_bytes().to_vec());
}

/// `dispatch_blocking` on a `current_thread` runtime must return a
/// clean `Err` rather than panicking inside `block_in_place`. The
/// API contract is `Result<_, String>`; a `current_thread` runtime
/// is unsupported but it must not blow up the dispatcher loop.
#[cfg(feature = "tokio")]
#[test]
fn async_hosted_rpc_dispatch_blocking_rejects_current_thread_runtime() {
    let cell = HostedRpcOwnerCell::from_async_owner(AsyncRpcCounter { n: 0 });
    let rt = ::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread tokio runtime");
    // `dispatch_blocking` is invoked *from* the current-thread
    // runtime so that `Handle::try_current()` resolves to it.
    let err = rt
        .block_on(async { cell.dispatch_blocking(1, &[]) })
        .expect_err("dispatch_blocking must reject current-thread runtimes cleanly");
    assert!(
        err.contains("multi-threaded"),
        "expected the rejection error to mention multi-threaded requirement, got '{err}'"
    );
}

/// `InProcessHostedRpcTransport` is the `--nocapture` / no-spawn
/// codepath. Under the tokio runner it must route to async owner
/// cells via the sync `call` -> `dispatch_blocking` bridge so the
/// in-process and IPC modes look identical to the user.
#[cfg(feature = "tokio")]
#[test]
fn async_hosted_rpc_in_process_transport_routes_to_async_cell() {
    use std::collections::HashMap;
    use std::sync::Arc;

    let dep_id = "in_process_async_owner".to_string();
    let cell = Arc::new(HostedRpcOwnerCell::from_async_owner(AsyncRpcCounter {
        n: 0,
    }));
    let mut cells = HashMap::new();
    cells.insert(dep_id.clone(), cell);

    let transport: Arc<dyn crate::internal::HostedRpcTransport> =
        Arc::new(InProcessHostedRpcTransport::new(cells));

    let rt = ::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let transport_clone = transport.clone();
    let dep_id_clone = dep_id.clone();
    let bytes = rt
        .block_on(async move {
            ::tokio::task::spawn_blocking(move || transport_clone.call(&dep_id_clone, 1, vec![]))
                .await
                .expect("spawn_blocking joined")
        })
        .expect("first in-process dispatch must succeed");
    assert_eq!(bytes, 1u64.to_be_bytes().to_vec());

    let transport_clone = transport.clone();
    let dep_id_clone = dep_id.clone();
    let bytes2 = rt
        .block_on(async move {
            ::tokio::task::spawn_blocking(move || transport_clone.call(&dep_id_clone, 1, vec![]))
                .await
                .expect("spawn_blocking joined")
        })
        .expect("second in-process dispatch must succeed");
    assert_eq!(bytes2, 2u64.to_be_bytes().to_vec());
}

#[test]
fn hosted_rpc_owner_panic_surfaces_then_poisons() {
    let cell = HostedRpcOwnerCell::from_owner(PanickingRpcOwner);

    // First call: the owner's `dispatch` panics with the literal string
    // "owner_panic_for_test"; `HostedRpcOwnerCell::dispatch` catches the
    // unwind and converts it into a textual error containing the panic
    // payload prefixed with "hosted rpc owner panicked: ".
    //
    // The catch_unwind catches the panic AFTER the MutexGuard has
    // started unwinding, which still leaves the mutex poisoned (verified
    // by direct std::sync::Mutex behaviour).
    let err1 = cell
        .dispatch(1, &[])
        .expect_err("first call must surface the panic as Err");
    assert!(
        err1.contains("hosted rpc owner panicked: owner_panic_for_test"),
        "expected first-call error to wrap the panic payload, got '{err1}'"
    );

    // Second call: the mutex is now poisoned from the panic above.
    // The cell must short-circuit with the stable "hosted rpc owner
    // poisoned" error and must NOT retry the owner.
    let err2 = cell
        .dispatch(1, &[])
        .expect_err("second call must short-circuit on the poisoned cell");
    assert_eq!(
        err2, "hosted rpc owner poisoned",
        "expected poisoned-cell error on the second call, got '{err2}'"
    );
}

// -------------------------------------------------------------
// Large-payload IPC framing coverage (>64 KiB) and
// concurrent in-flight RPC requests routed through the in-process
// transport without deadlock or framing corruption.
// -------------------------------------------------------------

#[test]
fn hosted_rpc_in_process_transport_round_trips_large_payload_exceeding_64_kib() {
    let counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_hosted_rpc_dep("rpc_dep", "", counter);
    let test = registered_test("t1", vec!["rpc_dep".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);
    let cells: HashMap<String, Arc<HostedRpcOwnerCell>> = execution
        .collect_hosted_rpc_owner_cells_sync()
        .into_iter()
        .collect();
    let transport: Arc<dyn HostedRpcTransport> = Arc::new(InProcessHostedRpcTransport::new(cells));
    let channel = HostedRpcChannel::new("tcrate::rpc_dep".to_string(), transport);
    let stub = <RpcCounter as HostedRpcDep>::build_stub(channel);

    const SIZE: u32 = 256 * 1024; // 256 KiB
    let bytes = stub.echo(SIZE);
    assert_eq!(
        bytes.len(),
        SIZE as usize,
        "framing dropped/truncated bytes"
    );
    for (i, b) in bytes.iter().enumerate() {
        assert_eq!(
            *b,
            (i % 251) as u8,
            "framing corrupted byte at index {i}: expected {}, got {b}",
            (i % 251) as u8
        );
    }
}

#[test]
fn hosted_rpc_in_process_transport_multiplexes_concurrent_calls_from_threads() {
    use std::thread;

    let counter = Arc::new(AtomicUsize::new(0));
    let dep = registered_hosted_rpc_dep("rpc_dep", "", counter);
    let test = registered_test("t1", vec!["rpc_dep".to_string()]);

    let (execution, _filtered) =
        TestSuiteExecution::construct(&Arguments::default(), &[dep], &[test], &[]);
    let cells: HashMap<String, Arc<HostedRpcOwnerCell>> = execution
        .collect_hosted_rpc_owner_cells_sync()
        .into_iter()
        .collect();
    let transport: Arc<dyn HostedRpcTransport> = Arc::new(InProcessHostedRpcTransport::new(cells));

    // Spawn N threads, each making M calls. Every call must return a
    // unique positive id (the owner is a single global counter
    // serialised by its own mutex). If the in-process transport ever
    // deadlocks or routes a reply to the wrong caller, the assertions
    // below would fire (duplicate ids, or the spawned thread would
    // panic and the join() would surface the failure).
    const N: usize = 4;
    const M: usize = 32;
    let mut handles = Vec::new();
    for _ in 0..N {
        let dep_id = "tcrate::rpc_dep".to_string();
        let transport = transport.clone();
        handles.push(thread::spawn(move || {
            let channel = HostedRpcChannel::new(dep_id, transport);
            let stub = <RpcCounter as HostedRpcDep>::build_stub(channel);
            let mut ids = Vec::with_capacity(M);
            for _ in 0..M {
                ids.push(stub.next());
            }
            ids
        }));
    }
    let mut all = Vec::with_capacity(N * M);
    for h in handles {
        all.extend(h.join().expect("thread panicked"));
    }
    all.sort();
    let mut prev: u64 = 0;
    for id in &all {
        assert!(
            *id > prev,
            "duplicate or non-monotonic id {id} after {prev}"
        );
        prev = *id;
    }
    assert_eq!(
        all.len(),
        N * M,
        "expected exactly {} ids in total, got {}",
        N * M,
        all.len()
    );
}
