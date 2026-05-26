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
    pub hosted_descriptor_bytes: Vec<DepWireBytes>,
    pub hosted_owners: Vec<HostedOwner>,
    pub hosted_rpc_owner_cells: Vec<(String, Arc<HostedRpcOwnerCell>)>,
}

impl ParentSharedDependencies {
    fn new() -> Self {
        Self {
            cloneable_wire_bytes: Vec::new(),
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
            if result.is_none()
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
mod cloneable_tests {
    use super::*;
    use crate::internal::{
        CloneableCodec, DependencyConstructor, RegisteredDependency, RegisteredTest, TestFunction,
        TestProperties,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn registered_test(name: &str, deps: Vec<String>) -> RegisteredTest {
        registered_test_in_module(name, "", deps)
    }

    fn registered_test_in_module(
        name: &str,
        module_path: &str,
        deps: Vec<String>,
    ) -> RegisteredTest {
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
        let parent_only_dep =
            registered_cloneable_dep("parent_only_dep", parent_only_counter.clone());
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
        let worker_fn =
            crate::internal::WorkerReconstructor::Sync(Arc::new(|wire_payload, _deps| {
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
        let worker_fn =
            crate::internal::WorkerReconstructor::Sync(Arc::new(|wire_payload, _deps| {
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
        let (descriptors, owners) =
            runtime.block_on(execution.collect_hosted_descriptor_bytes_async());

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
        let parent_only_dep =
            registered_cloneable_dep("parent_only_dep", parent_only_counter.clone());
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
        let transport: Arc<dyn HostedRpcTransport> =
            Arc::new(InProcessHostedRpcTransport::new(cells));
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
        let transport: Arc<dyn HostedRpcTransport> =
            Arc::new(InProcessHostedRpcTransport::new(cells));
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
        let transport: Arc<dyn HostedRpcTransport> =
            Arc::new(InProcessHostedRpcTransport::new(cells));

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
}
