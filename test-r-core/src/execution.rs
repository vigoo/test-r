use crate::args::Arguments;
use crate::internal::{
    filter_registered_tests, DependencyConstructor, DependencyView, RegisteredDependency,
    RegisteredTest,
};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use topological_sort::TopologicalSort;

pub(crate) struct TestSuiteExecution<'a> {
    crate_and_module: String,
    dependencies: Vec<&'a RegisteredDependency>,
    tests: Vec<&'a RegisteredTest>,
    inner: Vec<TestSuiteExecution<'a>>,
    materialized_dependencies: HashMap<String, Arc<dyn Any + Send + Sync>>,
    remaining_count: usize,
}

impl<'a> TestSuiteExecution<'a> {
    pub fn construct(
        arguments: &Arguments,
        dependencies: &'a [RegisteredDependency],
        tests: &'a [RegisteredTest],
    ) -> Self {
        let filtered_tests = filter_registered_tests(arguments, tests);

        if filtered_tests.is_empty() {
            Self::root(
                dependencies
                    .iter()
                    .filter(|dep| dep.crate_name.is_empty() && dep.module_path.is_empty())
                    .collect::<Vec<_>>(),
                Vec::new(),
            )
        } else {
            let mut root = Self::root(Vec::new(), Vec::new());

            for dep in dependencies {
                root.add_dependency(dep);
            }

            for test in tests {
                root.add_test(test);
            }

            root
        }
    }

    pub fn remaining(&self) -> usize {
        self.remaining_count
    }

    pub fn is_empty(&self) -> bool {
        self.tests.is_empty() && self.inner.is_empty()
    }

    #[cfg(feature = "tokio")]
    pub async fn pick_next(
        &mut self,
    ) -> Option<(&'a RegisteredTest, Box<dyn DependencyView + Send + Sync>)> {
        if self.is_empty() {
            None
        } else {
            match self
                .pick_next_internal(&self.create_dependency_map(&HashMap::new()))
                .await
            {
                Some((test, deps)) => Some((test, Box::new(deps))),
                None => None,
            }
        }
    }

    pub fn pick_next_sync(
        &mut self,
    ) -> Option<(&'a RegisteredTest, Box<dyn DependencyView + Send + Sync>)> {
        match self.pick_next_internal_sync(&HashMap::new()) {
            Some((test, deps)) => Some((test, Box::new(deps))),
            None => None,
        }
    }

    #[cfg(feature = "tokio")]
    async fn pick_next_internal(
        &mut self,
        materialized_parent_deps: &HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) -> Option<(
        &'a RegisteredTest,
        HashMap<String, Arc<dyn Any + Send + Sync>>,
    )> {
        if self.is_empty() {
            None
        } else {
            let dependency_map = if !self.is_materialized() {
                self.materialize_deps(materialized_parent_deps).await
            } else {
                self.create_dependency_map(materialized_parent_deps)
            };

            let result = if self.tests.is_empty() {
                let current = self.inner.iter_mut();
                let mut result = None;
                for inner in current {
                    if let Some((test, deps)) =
                        Box::pin(inner.pick_next_internal(&dependency_map)).await
                    {
                        result = Some((test, deps));
                        break;
                    }
                }
                self.inner.retain(|inner| !inner.is_empty());

                result
            } else if let Some(test) = self.tests.pop() {
                Some((test, dependency_map))
            } else {
                None
            };
            if result.is_none() && self.is_materialized() {
                self.drop_deps();
            }
            if result.is_some() {
                self.remaining_count -= 1;
            }
            result
        }
    }

    fn pick_next_internal_sync(
        &mut self,
        materialized_parent_deps: &HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) -> Option<(
        &'a RegisteredTest,
        HashMap<String, Arc<dyn Any + Send + Sync>>,
    )> {
        if self.is_empty() {
            None
        } else {
            let dependency_map = if !self.is_materialized() {
                self.materialize_deps_sync(materialized_parent_deps)
            } else {
                self.create_dependency_map(materialized_parent_deps)
            };

            let result = if self.tests.is_empty() {
                let current = self.inner.iter_mut();
                let mut result = None;
                for inner in current {
                    if let Some((test, deps)) = inner.pick_next_internal_sync(&dependency_map) {
                        result = Some((test, deps));
                        break;
                    }
                }

                self.inner.retain(|inner| !inner.is_empty());
                result
            } else if let Some(test) = self.tests.pop() {
                let deps = HashMap::new();
                Some((test, deps))
            } else {
                None
            };
            if result.is_none() && self.is_materialized() {
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

    fn root(deps: Vec<&'a RegisteredDependency>, tests: Vec<&'a RegisteredTest>) -> Self {
        let total_count = tests.len();
        Self {
            crate_and_module: String::new(),
            dependencies: deps,
            tests,
            inner: Vec::new(),
            materialized_dependencies: HashMap::new(),
            remaining_count: total_count,
        }
    }

    fn add_dependency(&mut self, dep: &'a RegisteredDependency) {
        let crate_and_module = dep.crate_and_module();
        if self.crate_and_module == crate_and_module {
            self.dependencies.push(dep);
        } else {
            let mut found = false;
            for inner in &mut self.inner {
                if Self::is_prefix_of(&inner.crate_and_module, &crate_and_module) {
                    inner.add_dependency(dep);
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
                    materialized_dependencies: HashMap::new(),
                    remaining_count: 0,
                };
                inner.add_dependency(dep);
                self.inner.push(inner);
            }
        }
    }

    fn add_test(&mut self, test: &'a RegisteredTest) {
        let crate_and_module = test.crate_and_module();
        if self.crate_and_module == crate_and_module {
            self.tests.push(test);
        } else {
            let mut found = false;
            for inner in &mut self.inner {
                if Self::is_prefix_of(&inner.crate_and_module, &crate_and_module) {
                    inner.add_test(test);
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
                    materialized_dependencies: HashMap::new(),
                    remaining_count: 0,
                };
                inner.add_test(test);
                self.inner.push(inner);
            }
        }
        self.remaining_count += 1;
    }

    fn is_materialized(&self) -> bool {
        self.materialized_dependencies.len() == self.dependencies.len()
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
                DependencyConstructor::Sync(cons) => cons(Box::new(dependency_map.clone())),
                DependencyConstructor::Async(cons) => cons(Box::new(dependency_map.clone())).await,
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
                DependencyConstructor::Sync(cons) => cons(Box::new(dependency_map.clone())),
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

    fn sorted_dependencies(&self) -> Vec<&'a RegisteredDependency> {
        let mut ts: TopologicalSort<&RegisteredDependency> = TopologicalSort::new();
        for dep in &self.dependencies {
            let mut added = false;
            for dep_dep_name in &dep.dependencies {
                if let Some(dep_dep) = self.dependencies.iter().find(|d| &d.name == dep_dep_name) {
                    ts.add_dependency(*dep_dep, *dep);
                    added = true;
                } else {
                    // otherwise it is expected to come from the parent level
                }
            }
            if !added {
                ts.insert(*dep);
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

impl<'a> Debug for TestSuiteExecution<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "'{}'", self.crate_and_module)?;
        writeln!(f, "  deps:")?;
        for dep in &self.dependencies {
            writeln!(f, "    '{}'", dep.name)?;
        }
        writeln!(f, "  tests:")?;
        for test in &self.tests {
            writeln!(f, "    '{}'", test.name)?;
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
