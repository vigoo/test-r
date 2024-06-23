use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use crate::args::Arguments;
use crate::internal::{
    filter_registered_tests, DependencyConstructor, DependencyView, RegisteredDependency,
    RegisteredTest,
};

pub(crate) struct TestSuiteExecution<'a> {
    crate_and_module: String,
    dependencies: Vec<&'a RegisteredDependency>,
    tests: Vec<&'a RegisteredTest>,
    inner: Vec<TestSuiteExecution<'a>>,
    materialized_dependencies: Vec<Arc<dyn Any + Send + Sync>>,
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

    #[cfg(feature = "tokio")]
    pub async fn pick_next(
        &mut self,
    ) -> Option<(&'a RegisteredTest, Box<dyn DependencyView + Send + Sync>)> {
        match self.pick_next_internal().await {
            Some((test, mut deps)) => {
                self.update_dep_map_with_missing(&mut deps).await;
                Some((test, Box::new(deps)))
            }
            None => None,
        }
    }

    pub fn pick_next_sync(
        &mut self,
    ) -> Option<(&'a RegisteredTest, Box<dyn DependencyView + Send + Sync>)> {
        match self.pick_next_internal_sync() {
            Some((test, mut deps)) => {
                self.update_dep_map_with_missing_sync(&mut deps);
                Some((test, Box::new(deps)))
            }
            None => None,
        }
    }

    #[cfg(feature = "tokio")]
    async fn pick_next_internal(
        &mut self,
    ) -> Option<(
        &'a RegisteredTest,
        HashMap<String, Arc<dyn Any + Send + Sync>>,
    )> {
        let result = if self.tests.is_empty() {
            let current = self.inner.iter_mut();
            let mut result = None;
            for inner in current {
                if let Some((test, mut deps)) = Box::pin(inner.pick_next_internal()).await {
                    self.update_dep_map_with_missing(&mut deps).await;
                    result = Some((test, deps));
                    break;
                }
            }
            result
        } else if let Some(test) = self.tests.pop() {
            let mut deps = HashMap::new();
            self.update_dep_map_with_missing(&mut deps).await;
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

    fn pick_next_internal_sync(
        &mut self,
    ) -> Option<(
        &'a RegisteredTest,
        HashMap<String, Arc<dyn Any + Send + Sync>>,
    )> {
        let result = if self.tests.is_empty() {
            let current = self.inner.iter_mut();
            let mut result = None;
            for inner in current {
                if let Some((test, mut deps)) = inner.pick_next_internal_sync() {
                    self.update_dep_map_with_missing_sync(&mut deps);
                    result = Some((test, deps));
                    break;
                }
            }
            result
        } else if let Some(test) = self.tests.pop() {
            let mut deps = HashMap::new();
            self.update_dep_map_with_missing_sync(&mut deps);
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

    #[cfg(feature = "tokio")]
    async fn update_dep_map_with_missing(
        &mut self,
        dep_map: &mut HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) {
        if !self.is_materialized() {
            self.materialize_deps().await;
        }
        for (idx, dep) in self.materialized_dependencies.iter().enumerate() {
            let key = self.dependencies[idx].name.clone();
            let dep = dep.clone();
            dep_map.entry(key).or_insert(dep);
        }
    }

    fn update_dep_map_with_missing_sync(
        &mut self,
        dep_map: &mut HashMap<String, Arc<dyn Any + Send + Sync>>,
    ) {
        if !self.is_materialized() {
            self.materialize_deps_sync();
        }
        for (idx, dep) in self.materialized_dependencies.iter().enumerate() {
            let key = self.dependencies[idx].name.clone();
            let dep = dep.clone();
            dep_map.entry(key).or_insert(dep);
        }
    }

    fn root(deps: Vec<&'a RegisteredDependency>, tests: Vec<&'a RegisteredTest>) -> Self {
        let total_count = tests.len();
        Self {
            crate_and_module: String::new(),
            dependencies: deps,
            tests,
            inner: Vec::new(),
            materialized_dependencies: Vec::new(),
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
                    materialized_dependencies: vec![],
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
                    materialized_dependencies: vec![],
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
    async fn materialize_deps(&mut self) {
        let mut deps = Vec::with_capacity(self.dependencies.len());
        for dep in &self.dependencies {
            match &dep.constructor {
                DependencyConstructor::Sync(cons) => {
                    deps.push(cons());
                }
                DependencyConstructor::Async(cons) => {
                    deps.push(cons().await);
                }
            }
        }
        self.materialized_dependencies = deps;
    }

    fn materialize_deps_sync(&mut self) {
        let mut deps = Vec::with_capacity(self.dependencies.len());
        for dep in &self.dependencies {
            match &dep.constructor {
                DependencyConstructor::Sync(cons) => {
                    deps.push(cons());
                }
                DependencyConstructor::Async(_cons) => {
                    panic!("Async dependencies are not supported in sync mode")
                }
            }
        }
        self.materialized_dependencies = deps;
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
    fn get(&self, name: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.get(name).cloned()
    }
}
