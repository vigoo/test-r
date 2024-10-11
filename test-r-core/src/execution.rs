use rand::prelude::{SliceRandom, StdRng};
use rand::SeedableRng;
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use topological_sort::TopologicalSort;

use crate::args::Arguments;
use crate::internal::{
    apply_suite_tags, filter_registered_tests, DependencyConstructor, DependencyView,
    RegisteredDependency, RegisteredTest, RegisteredTestSuiteProperty,
};

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
}

impl TestSuiteExecution {
    pub fn construct(
        arguments: &Arguments,
        dependencies: &[RegisteredDependency],
        tests: &[RegisteredTest],
        props: &[RegisteredTestSuiteProperty],
    ) -> Self {
        let tagged_tests = apply_suite_tags(tests, props);
        let mut filtered_tests = filter_registered_tests(arguments, &tagged_tests);
        Self::shuffle(arguments, &mut filtered_tests);
        filtered_tests.reverse();

        if filtered_tests.is_empty() {
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
            )
        } else {
            let mut root = Self::root(Vec::new(), Vec::new(), Vec::new());

            for prop in props {
                root.add_prop(prop.clone());
            }

            for dep in dependencies {
                root.add_dependency(dep.clone());
            }

            for test in filtered_tests {
                root.add_test(test.clone());
            }

            root
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
    pub fn has_dependencies(&self) -> bool {
        !self.dependencies.is_empty() || self.inner.iter().any(|inner| inner.has_dependencies())
    }

    /// Returns true if there are any tests that require capturing, based on the given default setting
    /// and the per-test CaptureControl overrides.
    pub fn requires_capturing(&self, capture_by_default: bool) -> bool {
        self.tests
            .iter()
            .any(|test| test.capture_control.requires_capturing(capture_by_default))
            || self
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
                Some((test, deps, seq_lock)) => {
                    let index = self.idx;
                    self.idx += 1;
                    Some(TestExecution {
                        test: test.clone(),
                        deps: Arc::new(deps),
                        index,
                        _seq_lock: seq_lock,
                    })
                }
                None => None,
            }
        }
    }

    pub fn pick_next_sync(&mut self) -> Option<TestExecution> {
        match self.pick_next_internal_sync(&HashMap::new()) {
            Some((test, deps, seq_lock)) => {
                let index = self.idx;
                self.idx += 1;
                Some(TestExecution {
                    test: test.clone(),
                    deps: Arc::new(deps),
                    index,
                    _seq_lock: seq_lock,
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
                    if let Some((test, deps, seq_lock)) =
                        Box::pin(inner.pick_next_internal(&dependency_map)).await
                    {
                        result = Some((test, deps, seq_lock));
                        break;
                    }
                }
                self.inner.retain(|inner| !inner.is_empty());

                result
            } else {
                let guard = self.sequential_lock.lock(self.is_sequential).await;
                self.tests.pop().map(|test| (test, dependency_map, guard))
            };
            if result.is_none() && self.is_materialized() && !locked {
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
                    if let Some((test, deps, seq_lock)) =
                        inner.pick_next_internal_sync(&dependency_map)
                    {
                        result = Some((test, deps, seq_lock));
                        break;
                    }
                }

                self.inner.retain(|inner| !inner.is_empty());
                result
            } else {
                let guard = self.sequential_lock.lock_sync(self.is_sequential);
                self.tests.pop().map(|test| (test, dependency_map, guard))
            };
            if result.is_none() && self.is_materialized() && !locked {
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
        let mut deps = HashMap::with_capacity(self.dependencies.len());
        let mut dependency_map = parent_map.clone();

        let sorted_dependencies = self.sorted_dependencies();
        for dep in &sorted_dependencies {
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
        let mut deps = HashMap::with_capacity(self.dependencies.len());
        let mut dependency_map = parent_map.clone();

        let sorted_dependencies = self.sorted_dependencies();
        for dep in &sorted_dependencies {
            let materialized_dep = match &dep.constructor {
                DependencyConstructor::Sync(cons) => cons(Arc::new(dependency_map.clone())),
                DependencyConstructor::Async(_cons) => {
                    panic!("Async dependencies are not supported in sync mode")
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
            writeln!(f, "    '{}' [{:?}]", test.name, test.test_type)?;
        }
        for inner in &self.inner {
            let inner_str = format!("{inner:?}");
            for inner_line in inner_str.lines() {
                writeln!(f, "  {}", inner_line)?;
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
}

#[allow(dead_code)]
enum SequentialExecutionLockGuard {
    None,
    #[cfg(feature = "tokio")]
    Async(tokio::sync::OwnedMutexGuard<()>),
    Sync(parking_lot::ArcMutexGuard<parking_lot::RawMutex, ()>),
}

struct SequentialExecutionLock {
    #[cfg(feature = "tokio")]
    async_mutex: Option<Arc<tokio::sync::Mutex<()>>>,
    sync_mutex: Option<Arc<parking_lot::Mutex<()>>>,
}

impl SequentialExecutionLock {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "tokio")]
            async_mutex: None,
            sync_mutex: None,
        }
    }

    #[cfg(feature = "tokio")]
    pub async fn is_locked(&self) -> bool {
        if let Some(mutex) = &self.async_mutex {
            mutex.try_lock().is_err()
        } else {
            false
        }
    }

    pub fn is_locked_sync(&self) -> bool {
        if let Some(mutex) = &self.sync_mutex {
            mutex.try_lock().is_none()
        } else {
            false
        }
    }

    #[cfg(feature = "tokio")]
    pub async fn lock(&mut self, is_sequential: bool) -> SequentialExecutionLockGuard {
        if is_sequential {
            if self.async_mutex.is_none() {
                self.async_mutex = Some(Arc::new(tokio::sync::Mutex::new(())));
            }

            let permit =
                tokio::sync::Mutex::lock_owned(self.async_mutex.as_ref().unwrap().clone()).await;
            SequentialExecutionLockGuard::Async(permit)
        } else {
            SequentialExecutionLockGuard::None
        }
    }

    pub fn lock_sync(&mut self, is_sequential: bool) -> SequentialExecutionLockGuard {
        if is_sequential {
            if self.sync_mutex.is_none() {
                self.sync_mutex = Some(Arc::new(parking_lot::Mutex::new(())));
            }

            let permit = parking_lot::Mutex::lock_arc(&self.sync_mutex.as_ref().unwrap().clone());
            SequentialExecutionLockGuard::Sync(permit)
        } else {
            SequentialExecutionLockGuard::None
        }
    }
}
