use rand::prelude::{SliceRandom, StdRng};
use rand::SeedableRng;
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use topological_sort::TopologicalSort;

use crate::args::Arguments;
use crate::internal::{
    apply_suite_props_to_tests, filter_registered_tests, DepScope, DependencyConstructor,
    DependencyView, HostedRpcOwnerCell, RegisteredDependency, RegisteredTest,
    RegisteredTestSuiteProperty,
};

/// Wire bytes for a single Cloneable / Hosted dependency, keyed by its
/// fully-qualified id (`{crate}::{module}::{name}`).
pub type DepWireBytes = (String, Vec<u8>);

/// Parent-held owner value (used only for `Hosted` deps — the parent keeps
/// the owner alive for the suite's duration).
pub type HostedOwner = Arc<dyn Any + Send + Sync>;

#[cfg(feature = "tokio")]
type ParentSharedDependenciesFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = HashMap<String, Arc<dyn Any + Send + Sync>>> + 'a>,
>;

#[cfg(test)]
/// Result of [`TestSuiteExecution::collect_hosted_descriptor_bytes_sync`] /
/// [`TestSuiteExecution::collect_hosted_descriptor_bytes_async`]: the
/// descriptor bytes that get shipped to workers, plus the parent-held owner
/// values that must outlive every worker.
pub type HostedDescriptorCollection = (Vec<DepWireBytes>, Vec<HostedOwner>);

/// Parent-side materialisation output for dependency scopes whose worker-side
/// value is derived from parent-owned state instead of by rerunning the user
/// constructor in each worker process.
pub struct ParentSharedDependencies {
    pub cloneable_wire_bytes: Vec<DepWireBytes>,
    /// Parent-constructed `Cloneable` values keyed by fully-qualified dep id.
    /// In **no-spawn-workers** mode (e.g. `--nocapture`) the runner installs
    /// these directly into the execution tree via
    /// [`TestSuiteExecution::provide_cloneable_value`] so tests see the
    /// parent's value without re-running the constructor in
    /// `materialize_deps`. For Cloneable, the round-trip
    /// `from_wire(to_wire(value))` is by contract semantics-preserving, so
    /// reusing the parent value directly is equivalent to round-tripping
    /// while avoiding the duplicate constructor run that historically
    /// occurred on the no-spawn-workers code path.
    ///
    /// In spawn-workers mode this list is unused — workers receive
    /// `cloneable_wire_bytes` over IPC instead.
    pub cloneable_local_values: Vec<(String, Arc<dyn Any + Send + Sync>)>,
    pub hosted_descriptor_bytes: Vec<DepWireBytes>,
    pub hosted_owners: Vec<HostedOwner>,
    pub hosted_rpc_owner_cells: Vec<(String, Arc<HostedRpcOwnerCell>)>,
}

impl ParentSharedDependencies {
    fn new() -> Self {
        Self {
            cloneable_wire_bytes: Vec::new(),
            cloneable_local_values: Vec::new(),
            hosted_descriptor_bytes: Vec::new(),
            hosted_owners: Vec::new(),
            hosted_rpc_owner_cells: Vec::new(),
        }
    }
}

pub(crate) struct TestSuiteExecution {
    crate_and_module: String,
    dependencies: Vec<RegisteredDependency>,
    tests: Vec<RegisteredTest>,
    props: Vec<RegisteredTestSuiteProperty>,
    inner: Vec<TestSuiteExecution>,
    materialized_dependencies: HashMap<String, Arc<dyn Any + Send + Sync>>,
    sequential_lock: SequentialExecutionLock,
    remaining_count: usize,
    idx: usize,
    is_sequential: bool,
    skip_creating_dependencies: bool,
    in_progress: Arc<AtomicUsize>,
}

impl TestSuiteExecution {
    pub fn construct(
        arguments: &Arguments,
        dependencies: &[RegisteredDependency],
        tests: &[RegisteredTest],
        props: &[RegisteredTestSuiteProperty],
    ) -> (Self, Vec<RegisteredTest>) {
        let tests_with_props = apply_suite_props_to_tests(tests, props);
        let mut filtered_tests = filter_registered_tests(arguments, &tests_with_props);
        Self::shuffle(arguments, &mut filtered_tests);
        filtered_tests.reverse();

        if filtered_tests.is_empty() {
            (
                Self::root(
                    dependencies
                        .iter()
                        .filter(|dep| dep.crate_name.is_empty() && dep.module_path.is_empty())
                        .cloned()
                        .collect::<Vec<_>>(),
                    Vec::new(),
                    props
                        .iter()
                        .filter(|dep| dep.crate_name().is_empty() && dep.module_path().is_empty())
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                Vec::new(),
            )
        } else {
            let mut root = Self::root(Vec::new(), Vec::new(), Vec::new());

            for prop in props {
                root.add_prop(prop.clone());
            }

            for dep in dependencies {
                root.add_dependency(dep.clone());
            }

            for test in filtered_tests.clone() {
                root.add_test(test.clone());
            }

            root.propagate_sequential(None);
            root.prune_unused_deps();

            (root, filtered_tests)
        }
    }

    fn shuffle(arguments: &Arguments, tests: &mut [RegisteredTest]) {
        if let Some(seed) = arguments.shuffle_seed {
            let mut rng = StdRng::seed_from_u64(seed);
            tests.shuffle(&mut rng);
        }
    }

    /// Disables creating dependencies when picking the next test. This is useful when the execution plan
    /// is only used to drive spawned workers instead of actually running the tests.
    pub fn skip_creating_dependencies(&mut self) {
        self.skip_creating_dependencies = true;
        for inner in &mut self.inner {
            inner.skip_creating_dependencies();
        }
    }

    pub fn remaining(&self) -> usize {
        self.remaining_count
    }

    pub fn is_empty(&self) -> bool {
        self.tests.is_empty() && self.inner.is_empty()
    }

    pub fn is_done(&self) -> bool {
        self.remaining_count == 0
    }

    /// Returns true if either this level, or any of the inner levels have dependencies
    #[allow(dead_code)]
    pub fn has_dependencies(&self) -> bool {
        !self.dependencies.is_empty() || self.inner.iter().any(|inner| inner.has_dependencies())
    }

    /// Returns true if any dependency in this subtree uses `DepScope::Shared`
    /// — those force single-threaded execution when output capture is on,
    /// because the materialised value cannot cross the parent/worker boundary.
    pub fn has_shared_dependencies(&self) -> bool {
        self.dependencies
            .iter()
            .any(|d| d.scope == DepScope::Shared)
            || self
                .inner
                .iter()
                .any(|inner| inner.has_shared_dependencies())
    }

    /// Returns true if any dependency in this subtree uses `DepScope::Cloneable`.
    #[allow(dead_code)]
    pub fn has_cloneable_dependencies(&self) -> bool {
        self.dependencies
            .iter()
            .any(|d| d.scope == DepScope::Cloneable)
            || self
                .inner
                .iter()
                .any(|inner| inner.has_cloneable_dependencies())
    }

    /// Returns true if any dependency in this subtree uses `DepScope::Hosted`.
    /// The parent keeps Hosted owners alive for the duration of the suite
    /// while shipping descriptors to workers.
    #[allow(dead_code)]
    pub fn has_hosted_dependencies(&self) -> bool {
        self.dependencies
            .iter()
            .any(|d| d.scope == DepScope::Hosted)
            || self
                .inner
                .iter()
                .any(|inner| inner.has_hosted_dependencies())
    }

    /// Returns true if any dependency in this subtree uses `DepScope::HostedRpc`.
    /// The parent keeps owner cells alive for the suite and routes
    /// worker-initiated IPC calls to those cells.
    #[allow(dead_code)]
    pub fn has_hosted_rpc_dependencies(&self) -> bool {
        self.dependencies
            .iter()
            .any(|d| d.scope == DepScope::HostedRpc)
            || self
                .inner
                .iter()
                .any(|inner| inner.has_hosted_rpc_dependencies())
    }

    /// Collects every Cloneable dependency in this subtree (depth-first).
    #[allow(dead_code)]
    pub fn collect_cloneable_dependencies(&self) -> Vec<RegisteredDependency> {
        let mut out = Vec::new();
        self.collect_cloneable_dependencies_into(&mut out);
        out
    }

    #[allow(dead_code)]
    fn collect_cloneable_dependencies_into(&self, out: &mut Vec<RegisteredDependency>) {
        for dep in &self.dependencies {
            if dep.scope == DepScope::Cloneable {
                out.push(dep.clone());
            }
        }
        for inner in &self.inner {
            inner.collect_cloneable_dependencies_into(out);
        }
    }

    /// Walks the subtree, materialising dependencies in dependency order and
    /// collecting the parent-side wire/state needed by Cloneable, Hosted, and
    /// HostedRpc scopes. Constructor dependencies are resolved in this parent
    /// context, but workers still receive these shared scopes as dependency-free
    /// leaves: Cloneable/Hosted values are reconstructed from bytes, and
    /// HostedRpc values are stubs backed by a channel.
    pub fn collect_parent_shared_dependencies_sync(&self) -> ParentSharedDependencies {
        let mut out = ParentSharedDependencies::new();
        let parent_map = HashMap::new();
        self.collect_parent_shared_dependencies_into_sync(&parent_map, &mut out);
        out
    }

    fn collect_parent_shared_dependencies_into_sync(
        &self,
        parent_map: &HashMap<String, Arc<dyn Any + Send + Sync>>,
        out: &mut ParentSharedDependencies,
    ) -> HashMap<String, Arc<dyn Any + Send + Sync>> {
        let mut dependency_map = parent_map.clone();
        let sorted_dependencies = self.sorted_dependencies();

        for dep in sorted_dependencies {
            if dependency_map.contains_key(&dep.name) {
                continue;
            }

            let value = Self::construct_dependency_sync(dep, &dependency_map);
            match dep.scope {
                DepScope::Cloneable => {
                    let codec = dep.cloneable_codec.as_ref().unwrap_or_else(|| {
                        panic!("Cloneable dep '{}' missing CloneableCodec", dep.name)
                    });
                    out.cloneable_wire_bytes
                        .push((dep.qualified_id(), (codec.to_wire)(value.clone())));
                    // Keep the parent-constructed value too, for the
                    // no-spawn-workers code path that installs Cloneable
                    // values directly into the execution tree (instead of
                    // re-running the constructor inside `materialize_deps`).
                    out.cloneable_local_values
                        .push((dep.qualified_id(), value.clone()));
                }
                DepScope::Hosted => {
                    let codec = dep.hosted_codec.as_ref().unwrap_or_else(|| {
                        panic!("Hosted dep '{}' missing hosted codec", dep.name)
                    });
                    out.hosted_descriptor_bytes
                        .push((dep.qualified_id(), (codec.to_wire)(value.clone())));
                    out.hosted_owners.push(value.clone());
                }
                DepScope::HostedRpc => {
                    let factory = dep.rpc_factory.as_ref().unwrap_or_else(|| {
                        panic!("HostedRpc dep '{}' missing RpcFactory", dep.name)
                    });
                    let cell = (factory.owner_into_cell)(value.clone());
                    out.hosted_rpc_owner_cells.push((dep.qualified_id(), cell));
                }
                DepScope::Shared | DepScope::PerWorker => {}
            }

            dependency_map.insert(dep.name.clone(), value);
        }

        for inner in &self.inner {
            inner.collect_parent_shared_dependencies_into_sync(&dependency_map, out);
        }

        dependency_map
    }

    fn construct_dependency_sync(
        dep: &RegisteredDependency,
        dependency_map: &HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) -> Arc<dyn Any + Send + Sync> {
        match &dep.constructor {
            DependencyConstructor::Sync(cons) => cons(Arc::new(dependency_map.clone())),
            DependencyConstructor::Async(cons) => {
                futures::executor::block_on(cons(Arc::new(dependency_map.clone())))
            }
        }
    }

    /// Collects only Cloneable wire bytes. The runner uses
    /// [`Self::collect_parent_shared_dependencies_sync`] to collect all shared
    /// parent-side values in one pass; this narrower helper remains for unit
    /// tests and focused callers.
    #[cfg(test)]
    pub fn collect_cloneable_wire_bytes_sync(&self) -> Vec<(String, Vec<u8>)> {
        self.collect_parent_shared_dependencies_sync()
            .cloneable_wire_bytes
    }

    /// Parent-side materialisation for `Hosted` dependencies.
    ///
    /// The returned descriptor bytes are keyed by fully-qualified dep id and
    /// the returned owner values must be kept alive for the duration of the
    /// suite. Unlike Cloneable, Hosted owners may hold resources (TCP
    /// listeners, Docker containers, gRPC clients, etc.) that workers'
    /// reconstructed handles depend on.
    #[cfg(test)]
    pub fn collect_hosted_descriptor_bytes_sync(&self) -> HostedDescriptorCollection {
        let collected = self.collect_parent_shared_dependencies_sync();
        (collected.hosted_descriptor_bytes, collected.hosted_owners)
    }

    /// Parent-side materialisation for `HostedRpc` dependencies.
    ///
    /// Returns `(qualified_id, cell)` pairs that the runtime keeps alive for
    /// the suite's lifetime and uses to dispatch worker-initiated RPC calls.
    #[cfg(test)]
    pub fn collect_hosted_rpc_owner_cells_sync(&self) -> Vec<(String, Arc<HostedRpcOwnerCell>)> {
        self.collect_parent_shared_dependencies_sync()
            .hosted_rpc_owner_cells
    }

    /// Async counterpart of [`Self::collect_parent_shared_dependencies_sync`].
    /// Async constructors are awaited on the parent before workers receive
    /// wire bytes, descriptors, or RPC stubs.
    #[cfg(feature = "tokio")]
    pub async fn collect_parent_shared_dependencies_async(&self) -> ParentSharedDependencies {
        let mut out = ParentSharedDependencies::new();
        let parent_map = HashMap::new();
        self.collect_parent_shared_dependencies_into_async(&parent_map, &mut out)
            .await;
        out
    }

    #[cfg(feature = "tokio")]
    fn collect_parent_shared_dependencies_into_async<'a>(
        &'a self,
        parent_map: &'a HashMap<String, Arc<dyn Any + Send + Sync>>,
        out: &'a mut ParentSharedDependencies,
    ) -> ParentSharedDependenciesFuture<'a> {
        Box::pin(async move {
            let mut dependency_map = parent_map.clone();
            let sorted_dependencies = self.sorted_dependencies();

            for dep in sorted_dependencies {
                if dependency_map.contains_key(&dep.name) {
                    continue;
                }

                let value = match &dep.constructor {
                    DependencyConstructor::Sync(cons) => cons(Arc::new(dependency_map.clone())),
                    DependencyConstructor::Async(cons) => {
                        cons(Arc::new(dependency_map.clone())).await
                    }
                };
                match dep.scope {
                    DepScope::Cloneable => {
                        let codec = dep.cloneable_codec.as_ref().unwrap_or_else(|| {
                            panic!("Cloneable dep '{}' missing CloneableCodec", dep.name)
                        });
                        out.cloneable_wire_bytes
                            .push((dep.qualified_id(), (codec.to_wire)(value.clone())));
                        // Keep the parent-constructed value too, for the
                        // no-spawn-workers code path that installs Cloneable
                        // values directly into the execution tree (instead
                        // of re-running the constructor inside
                        // `materialize_deps`).
                        out.cloneable_local_values
                            .push((dep.qualified_id(), value.clone()));
                    }
                    DepScope::Hosted => {
                        let codec = dep.hosted_codec.as_ref().unwrap_or_else(|| {
                            panic!("Hosted dep '{}' missing hosted codec", dep.name)
                        });
                        out.hosted_descriptor_bytes
                            .push((dep.qualified_id(), (codec.to_wire)(value.clone())));
                        out.hosted_owners.push(value.clone());
                    }
                    DepScope::HostedRpc => {
                        let factory = dep.rpc_factory.as_ref().unwrap_or_else(|| {
                            panic!("HostedRpc dep '{}' missing RpcFactory", dep.name)
                        });
                        let cell = (factory.owner_into_cell)(value.clone());
                        out.hosted_rpc_owner_cells.push((dep.qualified_id(), cell));
                    }
                    DepScope::Shared | DepScope::PerWorker => {}
                }

                dependency_map.insert(dep.name.clone(), value);
            }

            for inner in &self.inner {
                inner
                    .collect_parent_shared_dependencies_into_async(&dependency_map, out)
                    .await;
            }
            dependency_map
        })
    }

    /// Async Hosted-only collection helper retained for focused callers.
    #[cfg(feature = "tokio")]
    #[cfg(test)]
    pub async fn collect_hosted_descriptor_bytes_async(&self) -> HostedDescriptorCollection {
        let collected = self.collect_parent_shared_dependencies_async().await;
        (collected.hosted_descriptor_bytes, collected.hosted_owners)
    }

    /// Async Cloneable-only collection helper retained for focused callers.
    ///
    /// **Intentionally `!Send`.** The underlying `DependencyConstructor::Async`
    /// future is not `Send`, so the returned future from this collector cannot
    /// be either. Must be awaited on the root runner task (i.e., under
    /// `Runtime::block_on` or directly inside `test_runner`) — never inside
    /// `tokio::spawn` / a `JoinSet`. If we ever want to spawn Cloneable
    /// collection onto a worker, the constructor type would need to require
    /// `Send` first.
    #[cfg(feature = "tokio")]
    #[cfg(test)]
    pub async fn collect_cloneable_wire_bytes_async(&self) -> Vec<(String, Vec<u8>)> {
        self.collect_parent_shared_dependencies_async()
            .await
            .cloneable_wire_bytes
    }

    /// Worker-side counterpart to [`Self::collect_cloneable_wire_bytes_sync`]:
    /// pre-populates the Cloneable dep value at the node where the dep is
    /// registered, so the upcoming `materialize_deps_sync` call uses the
    /// provided value instead of running the original constructor. The lookup
    /// is keyed by the dep's fully-qualified id
    /// (`{crate}::{module}::{name}`), but the value is stored under the local
    /// `name` so the rest of the materialisation logic keeps working unchanged.
    /// Returns `true` if a matching dep was found in any node of the subtree.
    pub fn provide_cloneable_value(
        &mut self,
        dep_id: &str,
        value: Arc<dyn Any + Send + Sync>,
    ) -> bool {
        let applied = self.provide_cloneable_value_internal(dep_id, value);
        if applied {
            self.prune_unused_deps();
        }
        applied
    }

    fn provide_cloneable_value_internal(
        &mut self,
        dep_id: &str,
        value: Arc<dyn Any + Send + Sync>,
    ) -> bool {
        let mut applied = false;
        if let Some((local_name, dep_idx)) = self
            .dependencies
            .iter()
            .enumerate()
            .find(|(_, d)| d.qualified_id() == dep_id)
            .map(|(idx, d)| (d.name.clone(), idx))
        {
            // From the worker execution tree's perspective this dependency is
            // now a leaf: its value came from wire bytes or a HostedRpc channel,
            // so the worker must not instantiate constructor-only dependencies
            // that were needed solely in the parent collection context.
            self.dependencies[dep_idx].dependencies.clear();
            self.materialized_dependencies
                .insert(local_name, value.clone());
            applied = true;
        }
        for inner in &mut self.inner {
            applied |= inner.provide_cloneable_value_internal(dep_id, value.clone());
        }
        applied
    }

    /// Returns true if there are any tests that require capturing, based on the given default setting
    /// and the per-test CaptureControl overrides.
    pub fn requires_capturing(&self, capture_by_default: bool) -> bool {
        self.tests.iter().any(|test| {
            test.props
                .capture_control
                .requires_capturing(capture_by_default)
        }) || self
            .inner
            .iter()
            .any(|inner| inner.requires_capturing(capture_by_default))
    }

    #[cfg(feature = "tokio")]
    pub async fn pick_next(&mut self) -> Option<TestExecution> {
        if self.is_empty() {
            None
        } else {
            match self
                .pick_next_internal(&self.create_dependency_map(&HashMap::new()))
                .await
            {
                Some((test, deps, seq_lock, in_progress_counter)) => {
                    let index = self.idx;
                    self.idx += 1;
                    Some(TestExecution {
                        test: test.clone(),
                        deps: Arc::new(deps),
                        index,
                        _seq_lock: seq_lock,
                        in_progress_counter,
                    })
                }
                None => None,
            }
        }
    }

    pub fn pick_next_sync(&mut self) -> Option<TestExecution> {
        match self.pick_next_internal_sync(&HashMap::new()) {
            Some((test, deps, seq_lock, in_progress_counter)) => {
                let index = self.idx;
                self.idx += 1;
                Some(TestExecution {
                    test: test.clone(),
                    deps: Arc::new(deps),
                    index,
                    _seq_lock: seq_lock,
                    in_progress_counter,
                })
            }
            None => None,
        }
    }

    #[cfg(feature = "tokio")]
    #[allow(clippy::type_complexity)]
    async fn pick_next_internal(
        &mut self,
        materialized_parent_deps: &HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) -> Option<(
        RegisteredTest,
        HashMap<String, Arc<dyn Any + Send + Sync>>,
        SequentialExecutionLockGuard,
        Arc<AtomicUsize>,
    )> {
        if self.is_empty() {
            None
        } else {
            let dependency_map = if !self.is_materialized() {
                self.materialize_deps(materialized_parent_deps).await
            } else {
                self.create_dependency_map(materialized_parent_deps)
            };

            let locked = self.sequential_lock.is_locked().await;
            let result = if self.tests.is_empty() || locked {
                let current = self.inner.iter_mut();
                let mut result = None;
                for inner in current {
                    if let Some((test, deps, seq_lock, in_progress_counter)) =
                        Box::pin(inner.pick_next_internal(&dependency_map)).await
                    {
                        result = Some((test, deps, seq_lock, in_progress_counter));
                        break;
                    }
                }
                self.inner.retain(|inner| !inner.is_empty());

                result
            } else {
                let guard = self.sequential_lock.lock(self.is_sequential).await;
                self.in_progress.fetch_add(1, Ordering::Release);
                self.tests
                    .pop()
                    .map(|test| (test, dependency_map, guard, self.in_progress.clone()))
            };
            if result.is_none()
                && self.is_empty()
                && self.is_materialized()
                && !locked
                && self.in_progress.load(Ordering::Acquire) == 0
            {
                self.drop_deps();
            }
            if result.is_some() {
                self.remaining_count -= 1;
            }
            result
        }
    }

    #[allow(clippy::type_complexity)]
    fn pick_next_internal_sync(
        &mut self,
        materialized_parent_deps: &HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) -> Option<(
        RegisteredTest,
        HashMap<String, Arc<dyn Any + Send + Sync>>,
        SequentialExecutionLockGuard,
        Arc<AtomicUsize>,
    )> {
        if self.is_empty() {
            None
        } else {
            let dependency_map = if !self.is_materialized() {
                self.materialize_deps_sync(materialized_parent_deps)
            } else {
                self.create_dependency_map(materialized_parent_deps)
            };

            let locked = self.sequential_lock.is_locked_sync();
            let result = if self.tests.is_empty() || locked {
                let current = self.inner.iter_mut();
                let mut result = None;
                for inner in current {
                    if let Some((test, deps, seq_lock, in_progress_counter)) =
                        inner.pick_next_internal_sync(&dependency_map)
                    {
                        result = Some((test, deps, seq_lock, in_progress_counter));
                        break;
                    }
                }

                self.inner.retain(|inner| !inner.is_empty());
                result
            } else {
                let guard = self.sequential_lock.lock_sync(self.is_sequential);
                self.in_progress.fetch_add(1, Ordering::Release);
                self.tests
                    .pop()
                    .map(|test| (test, dependency_map, guard, self.in_progress.clone()))
            };
            // `is_empty()` matches `pick_next_internal`: a `None` result
            // can mean "descendant is temporarily locked", not "subtree
            // done" — dropping deps here would force rematerialisation.
            if result.is_none()
                && self.is_empty()
                && self.is_materialized()
                && !locked
                && self.in_progress.load(Ordering::Acquire) == 0
            {
                self.drop_deps();
            }
            if result.is_some() {
                self.remaining_count -= 1;
            }
            result
        }
    }

    fn create_dependency_map(
        &self,
        parent_map: &HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) -> HashMap<String, Arc<dyn Any + Send + Sync>> {
        let mut result = parent_map.clone();
        for (key, dep) in &self.materialized_dependencies {
            result.insert(key.clone(), dep.clone());
        }
        result
    }

    fn root(
        deps: Vec<RegisteredDependency>,
        tests: Vec<RegisteredTest>,
        props: Vec<RegisteredTestSuiteProperty>,
    ) -> Self {
        let total_count = tests.len();
        let is_sequential = props
            .iter()
            .any(|prop| matches!(prop, RegisteredTestSuiteProperty::Sequential { .. }))
            || tests.iter().any(|test| test.run.is_bench());
        Self {
            crate_and_module: String::new(),
            dependencies: deps,
            tests,
            props,
            inner: Vec::new(),
            materialized_dependencies: HashMap::new(),
            remaining_count: total_count,
            idx: 0,
            sequential_lock: SequentialExecutionLock::new(),
            is_sequential,
            skip_creating_dependencies: false,
            in_progress: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn add_dependency(&mut self, dep: RegisteredDependency) {
        let crate_and_module = dep.crate_and_module();
        if self.crate_and_module == crate_and_module {
            self.dependencies.push(dep);
        } else {
            let mut found = false;
            for inner in &mut self.inner {
                if Self::is_prefix_of(&inner.crate_and_module, &crate_and_module) {
                    inner.add_dependency(dep.clone());
                    found = true;
                    break;
                }
            }
            if !found {
                let mut inner = Self {
                    crate_and_module: Self::next_level(&self.crate_and_module, &crate_and_module),
                    dependencies: vec![],
                    tests: vec![],
                    inner: vec![],
                    props: vec![],
                    materialized_dependencies: HashMap::new(),
                    remaining_count: 0,
                    idx: 0,
                    is_sequential: false,
                    sequential_lock: SequentialExecutionLock::new(),
                    skip_creating_dependencies: false,
                    in_progress: Arc::new(AtomicUsize::new(0)),
                };
                inner.add_dependency(dep);
                self.inner.push(inner);
            }
        }
    }

    fn add_test(&mut self, test: RegisteredTest) {
        let crate_and_module = test.crate_and_module();
        if self.crate_and_module == crate_and_module {
            self.tests.push(test.clone());

            if test.run.is_bench() {
                self.is_sequential = true;
            }
        } else {
            let mut found = false;
            for inner in &mut self.inner {
                if Self::is_prefix_of(&inner.crate_and_module, &crate_and_module) {
                    inner.add_test(test.clone());
                    found = true;
                    break;
                }
            }
            if !found {
                let mut inner = Self {
                    crate_and_module: Self::next_level(&self.crate_and_module, &crate_and_module),
                    dependencies: vec![],
                    tests: vec![],
                    inner: vec![],
                    props: vec![],
                    materialized_dependencies: HashMap::new(),
                    remaining_count: 0,
                    idx: 0,
                    is_sequential: false,
                    sequential_lock: SequentialExecutionLock::new(),
                    skip_creating_dependencies: false,
                    in_progress: Arc::new(AtomicUsize::new(0)),
                };
                inner.add_test(test);
                self.inner.push(inner);
            }
        }
        self.remaining_count += 1;
    }

    fn add_prop(&mut self, prop: RegisteredTestSuiteProperty) {
        let crate_and_module = prop.crate_and_module();
        if self.crate_and_module == crate_and_module {
            if matches!(prop, RegisteredTestSuiteProperty::Sequential { .. }) {
                self.is_sequential = true;
            }
            self.props.push(prop);
        } else {
            let mut found = false;
            for inner in &mut self.inner {
                if Self::is_prefix_of(&inner.crate_and_module, &crate_and_module) {
                    inner.add_prop(prop.clone());
                    found = true;
                    break;
                }
            }
            if !found {
                let mut inner = Self {
                    crate_and_module: Self::next_level(&self.crate_and_module, &crate_and_module),
                    dependencies: vec![],
                    tests: vec![],
                    inner: vec![],
                    props: vec![],
                    materialized_dependencies: HashMap::new(),
                    remaining_count: 0,
                    idx: 0,
                    is_sequential: false,
                    sequential_lock: SequentialExecutionLock::new(),
                    skip_creating_dependencies: false,
                    in_progress: Arc::new(AtomicUsize::new(0)),
                };
                inner.add_prop(prop);
                self.inner.push(inner);
            }
        }
    }

    fn is_materialized(&self) -> bool {
        self.skip_creating_dependencies
            || self.materialized_dependencies.len() == self.dependencies.len()
    }

    #[cfg(feature = "tokio")]
    async fn materialize_deps(
        &mut self,
        parent_map: &HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) -> HashMap<String, Arc<dyn Any + Send + Sync>> {
        // Start with any pre-populated values (e.g. Cloneable deps received
        // from the parent via ProvideCloneable IPC).
        let mut deps = self.materialized_dependencies.clone();
        let mut dependency_map = parent_map.clone();
        for (k, v) in &deps {
            dependency_map.insert(k.clone(), v.clone());
        }

        let sorted_dependencies = self.sorted_dependencies();
        for dep in &sorted_dependencies {
            if deps.contains_key(&dep.name) {
                continue;
            }
            let materialized_dep = match &dep.constructor {
                DependencyConstructor::Sync(cons) => cons(Arc::new(dependency_map.clone())),
                DependencyConstructor::Async(cons) => cons(Arc::new(dependency_map.clone())).await,
            };
            deps.insert(dep.name.clone(), materialized_dep.clone());
            dependency_map.insert(dep.name.clone(), materialized_dep);
        }
        self.materialized_dependencies = deps;
        dependency_map
    }

    fn materialize_deps_sync(
        &mut self,
        parent_map: &HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) -> HashMap<String, Arc<dyn Any + Send + Sync>> {
        // Start with any pre-populated values (e.g. Cloneable deps received
        // from the parent via ProvideCloneable IPC).
        let mut deps = self.materialized_dependencies.clone();
        let mut dependency_map = parent_map.clone();
        for (k, v) in &deps {
            dependency_map.insert(k.clone(), v.clone());
        }

        let sorted_dependencies = self.sorted_dependencies();
        for dep in &sorted_dependencies {
            if deps.contains_key(&dep.name) {
                continue;
            }
            let materialized_dep = match &dep.constructor {
                DependencyConstructor::Sync(cons) => cons(Arc::new(dependency_map.clone())),
                DependencyConstructor::Async(cons) => {
                    futures::executor::block_on(cons(Arc::new(dependency_map.clone())))
                }
            };
            deps.insert(dep.name.clone(), materialized_dep.clone());
            dependency_map.insert(dep.name.clone(), materialized_dep);
        }
        self.materialized_dependencies = deps;
        dependency_map
    }

    fn sorted_dependencies(&self) -> Vec<&RegisteredDependency> {
        let mut ts: TopologicalSort<&RegisteredDependency> = TopologicalSort::new();
        for dep in &self.dependencies {
            let mut added = false;
            for dep_dep_name in &dep.dependencies {
                if let Some(dep_dep) = self.dependencies.iter().find(|d| &d.name == dep_dep_name) {
                    ts.add_dependency(dep_dep, dep);
                    added = true;
                } else {
                    // otherwise it is expected to come from the parent level
                }
            }
            if !added {
                ts.insert(dep);
            }
        }
        let mut result = Vec::with_capacity(self.dependencies.len());
        loop {
            let chunk = ts.pop_all();
            if chunk.is_empty() {
                break;
            }
            result.extend(chunk);
        }
        result
    }

    fn drop_deps(&mut self) {
        self.materialized_dependencies.clear();
    }

    /// Prunes dependencies that are not needed by any test in this subtree.
    /// Returns `Some(needed_from_parent)` with dep names needed from ancestor levels,
    /// or `None` if pruning is disabled for this subtree (unknown deps).
    fn prune_unused_deps(&mut self) -> Option<HashSet<String>> {
        // Collect dep names needed by tests at this level
        let mut needed: Option<HashSet<String>> = Some(HashSet::new());
        for test in &self.tests {
            match &test.dependencies {
                None => {
                    needed = None;
                    break;
                }
                Some(deps) => {
                    if let Some(ref mut set) = needed {
                        set.extend(deps.iter().cloned());
                    }
                }
            }
        }

        // Merge children's needs
        for inner in &mut self.inner {
            let child_needs = inner.prune_unused_deps();
            needed = match (needed, child_needs) {
                (None, _) | (_, None) => None,
                (Some(mut a), Some(b)) => {
                    a.extend(b);
                    Some(a)
                }
            };
        }

        // If any test has unknown deps, keep everything
        let needed = needed?;

        // Determine which local deps to keep
        let local_names: HashSet<String> =
            self.dependencies.iter().map(|d| d.name.clone()).collect();
        let mut keep_local: HashSet<String> = needed.intersection(&local_names).cloned().collect();

        // Expand transitive closure for local deps only (fixpoint)
        let mut queue: VecDeque<String> = keep_local.iter().cloned().collect();
        let mut needed_from_parent: HashSet<String> =
            needed.difference(&local_names).cloned().collect();

        while let Some(dep_name) = queue.pop_front() {
            if let Some(dep) = self.dependencies.iter().find(|d| d.name == dep_name) {
                for transitive in &dep.dependencies {
                    if local_names.contains(transitive) {
                        if keep_local.insert(transitive.clone()) {
                            queue.push_back(transitive.clone());
                        }
                    } else {
                        needed_from_parent.insert(transitive.clone());
                    }
                }
                // Companions are planner-only sibling links — no
                // constructor argument is derived from them, but they
                // must be retained together with the dep they are
                // declared on. Used by the
                // `#[test_dep(scope = Hosted, worker = both(T))]`
                // lowering to keep the Hosted owner half and the
                // HostedRpc stub half as a pair even when the
                // selected tests only parameterise on one of them.
                for companion in &dep.companions {
                    if local_names.contains(companion) {
                        if keep_local.insert(companion.clone()) {
                            queue.push_back(companion.clone());
                        }
                    } else {
                        needed_from_parent.insert(companion.clone());
                    }
                }
            }
        }

        // Prune
        self.dependencies.retain(|d| keep_local.contains(&d.name));

        Some(needed_from_parent)
    }

    fn is_prefix_of(this: &str, that: &str) -> bool {
        this.is_empty() || this == that || that.starts_with(&format!("{this}::"))
    }

    fn next_level(from: &str, to: &str) -> String {
        assert!(Self::is_prefix_of(from, to));
        let remaining = if from.is_empty() {
            to
        } else {
            &to[from.len() + 2..]
        };

        let result = if let Some((next, _tail)) = remaining.split_once("::") {
            format!("{from}::{next}")
        } else {
            format!("{from}::{remaining}")
        };
        result.trim_start_matches("::").to_string()
    }

    fn propagate_sequential(&mut self, inherited_lock: Option<&SequentialExecutionLock>) {
        if let Some(parent_lock) = inherited_lock {
            self.is_sequential = true;
            self.sequential_lock = parent_lock.clone();
        }

        let lock_for_children = if self.is_sequential {
            Some(self.sequential_lock.clone())
        } else {
            None
        };

        for child in &mut self.inner {
            child.propagate_sequential(lock_for_children.as_ref());
        }
    }
}

impl Debug for TestSuiteExecution {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "'{}' {} [{}]",
            self.crate_and_module,
            self.props
                .iter()
                .map(|x| format!("{x:?}"))
                .collect::<Vec<_>>()
                .join(", "),
            if self.is_sequential { "S" } else { "P" }
        )?;
        writeln!(f, "  deps:")?;
        for dep in &self.dependencies {
            writeln!(f, "    '{}'", dep.name)?;
        }
        writeln!(f, "  tests:")?;
        for test in &self.tests {
            writeln!(f, "    '{}' [{:?}]", test.name, test.props.test_type)?;
        }
        for inner in &self.inner {
            let inner_str = format!("{inner:?}");
            for inner_line in inner_str.lines() {
                writeln!(f, "  {inner_line}")?;
            }
        }
        Ok(())
    }
}

impl DependencyView for HashMap<String, Arc<dyn Any + Send + Sync>> {
    fn get(&self, name: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.get(name).cloned()
    }
}

pub struct TestExecution {
    pub test: RegisteredTest,
    pub deps: Arc<dyn DependencyView + Send + Sync>,
    pub index: usize,
    _seq_lock: SequentialExecutionLockGuard,
    in_progress_counter: Arc<AtomicUsize>,
}

impl Drop for TestExecution {
    fn drop(&mut self) {
        self.in_progress_counter.fetch_sub(1, Ordering::Release);
    }
}

#[allow(dead_code)]
enum SequentialExecutionLockGuard {
    None,
    #[cfg(feature = "tokio")]
    Async(tokio::sync::OwnedMutexGuard<()>),
    Sync(parking_lot::ArcMutexGuard<parking_lot::RawMutex, ()>),
}

#[derive(Clone)]
struct SequentialExecutionLock {
    #[cfg(feature = "tokio")]
    async_mutex: Arc<tokio::sync::Mutex<()>>,
    sync_mutex: Arc<parking_lot::Mutex<()>>,
}

impl SequentialExecutionLock {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "tokio")]
            async_mutex: Arc::new(tokio::sync::Mutex::new(())),
            sync_mutex: Arc::new(parking_lot::Mutex::new(())),
        }
    }

    #[cfg(feature = "tokio")]
    pub async fn is_locked(&self) -> bool {
        self.async_mutex.try_lock().is_err()
    }

    pub fn is_locked_sync(&self) -> bool {
        self.sync_mutex.try_lock().is_none()
    }

    #[cfg(feature = "tokio")]
    pub async fn lock(&self, is_sequential: bool) -> SequentialExecutionLockGuard {
        if is_sequential {
            let permit = tokio::sync::Mutex::lock_owned(self.async_mutex.clone()).await;
            SequentialExecutionLockGuard::Async(permit)
        } else {
            SequentialExecutionLockGuard::None
        }
    }

    pub fn lock_sync(&self, is_sequential: bool) -> SequentialExecutionLockGuard {
        if is_sequential {
            let permit = parking_lot::Mutex::lock_arc(&self.sync_mutex);
            SequentialExecutionLockGuard::Sync(permit)
        } else {
            SequentialExecutionLockGuard::None
        }
    }
}

#[cfg(test)]
mod tests;
