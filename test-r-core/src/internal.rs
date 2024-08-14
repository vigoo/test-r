use crate::args::Arguments;
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub enum TestFunction {
    Sync(Arc<dyn Fn(Box<dyn DependencyView + Send + Sync>) + Send + Sync + 'static>),
    Async(
        Arc<
            dyn (Fn(
                    Box<dyn DependencyView + Send + Sync>,
                ) -> Pin<Box<dyn Future<Output = ()> + Send>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

#[derive(Clone)]
pub struct RegisteredTest {
    pub name: String,
    pub crate_name: String,
    pub module_path: String,
    pub is_ignored: bool,
    pub run: TestFunction,
}

impl RegisteredTest {
    pub fn filterable_name(&self) -> String {
        if !self.module_path.is_empty() {
            format!("{}::{}", self.module_path, self.name)
        } else {
            self.name.clone()
        }
    }

    pub fn fully_qualified_name(&self) -> String {
        [&self.crate_name, &self.module_path, &self.name]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }

    pub fn crate_and_module(&self) -> String {
        [&self.crate_name, &self.module_path]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }
}

impl Debug for RegisteredTest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredTest")
            .field("name", &self.name)
            .field("crate_name", &self.crate_name)
            .field("module_path", &self.module_path)
            .finish()
    }
}

pub static REGISTERED_TESTS: Mutex<Vec<RegisteredTest>> = Mutex::new(Vec::new());

#[derive(Clone)]
pub enum DependencyConstructor {
    Sync(
        Arc<
            dyn (Fn(
                    Box<dyn DependencyView + Send + Sync>,
                ) -> Arc<dyn std::any::Any + Send + Sync + 'static>)
                + Send
                + Sync
                + 'static,
        >,
    ),
    Async(
        Arc<
            dyn (Fn(
                    Box<dyn DependencyView + Send + Sync>,
                )
                    -> Pin<Box<dyn Future<Output = Arc<dyn std::any::Any + Send + Sync>> + Send>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

pub struct RegisteredDependency {
    pub name: String, // TODO: Should we use TypeId here?
    pub crate_name: String,
    pub module_path: String,
    pub constructor: DependencyConstructor,
    pub dependencies: Vec<String>,
}

impl Debug for RegisteredDependency {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredDependency")
            .field("name", &self.name)
            .field("crate_name", &self.crate_name)
            .field("module_path", &self.module_path)
            .finish()
    }
}

impl PartialEq for RegisteredDependency {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for RegisteredDependency {}

impl Hash for RegisteredDependency {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl RegisteredDependency {
    pub fn crate_and_module(&self) -> String {
        [&self.crate_name, &self.module_path]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }
}

pub static REGISTERED_DEPENDENCY_CONSTRUCTORS: Mutex<Vec<RegisteredDependency>> =
    Mutex::new(Vec::new());

#[derive(Debug, Clone)]
pub enum RegisteredTestSuiteProperty {
    Sequential {
        name: String,
        crate_name: String,
        module_path: String,
    },
}

impl RegisteredTestSuiteProperty {
    pub fn crate_name(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { crate_name, .. } => crate_name,
        }
    }

    pub fn module_path(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { module_path, .. } => module_path,
        }
    }

    pub fn name(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { name, .. } => name,
        }
    }

    pub fn crate_and_module(&self) -> String {
        [self.crate_name(), self.module_path(), self.name()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }
}

pub static REGISTERED_TESTSUITE_PROPS: Mutex<Vec<RegisteredTestSuiteProperty>> =
    Mutex::new(Vec::new());

#[derive(Clone)]
pub enum TestGeneratorFunction {
    Sync(Arc<dyn Fn() -> Vec<TestFunction> + Send + Sync + 'static>),
    Async(
        Arc<
            dyn (Fn() -> Pin<Box<dyn Future<Output = Vec<TestFunction>> + Send>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

#[derive(Clone)]
pub struct RegisteredTestGenerator {
    pub name: String,
    pub crate_name: String,
    pub module_path: String,
    pub run: TestGeneratorFunction,
}

impl RegisteredTestGenerator {
    pub fn crate_and_module(&self) -> String {
        [&self.crate_name, &self.module_path]
            .into_iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<String>>()
            .join("::")
    }
}

pub static REGISTERED_TEST_GENERATORS: Mutex<Vec<RegisteredTestGenerator>> =
    Mutex::new(Vec::new());


pub(crate) fn filter_test(test: &RegisteredTest, filter: &str, exact: bool) -> bool {
    if exact {
        test.filterable_name() == filter
    } else {
        test.filterable_name().contains(filter)
    }
}

pub(crate) fn filter_registered_tests<'a>(
    args: &Arguments,
    registered_tests: &'a [RegisteredTest],
) -> Vec<&'a RegisteredTest> {
    registered_tests
        .iter()
        .filter(|registered_test| {
            args.filter.as_ref().is_none()
                || args
                    .filter
                    .as_ref()
                    .map(|filter| filter_test(registered_test, filter, args.exact))
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>()
}

pub enum TestResult {
    Passed,
    Failed {
        panic: Box<dyn std::any::Any + Send>,
    },
    Ignored,
}

impl TestResult {
    pub(crate) fn is_passed(&self) -> bool {
        matches!(self, TestResult::Passed)
    }

    pub(crate) fn is_failed(&self) -> bool {
        matches!(self, TestResult::Failed { .. })
    }

    pub(crate) fn is_ignored(&self) -> bool {
        matches!(self, TestResult::Ignored)
    }

    pub(crate) fn failure_message(&self) -> Option<&str> {
        match self {
            TestResult::Failed { panic } => panic
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or(panic.downcast_ref::<&str>().copied()),
            _ => None,
        }
    }
}

pub struct SuiteResult {
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub measured: usize,
    pub filtered_out: usize,
    pub exec_time: Duration,
}

impl SuiteResult {
    pub fn from_test_results(
        registered_tests: &[RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
    ) -> Self {
        let passed = results
            .iter()
            .filter(|(_, result)| result.is_passed())
            .count();
        let failed = results
            .iter()
            .filter(|(_, result)| result.is_failed())
            .count();
        let ignored = results
            .iter()
            .filter(|(_, result)| result.is_ignored())
            .count();
        let filtered_out = registered_tests.len() - results.len();

        Self {
            passed,
            failed,
            ignored,
            measured: 0,
            filtered_out,
            exec_time: Duration::new(0, 0),
        }
    }
}

pub trait DependencyView {
    fn get(&self, name: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>>;
}

impl DependencyView for Box<dyn DependencyView + Send + Sync> {
    fn get(&self, name: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.as_ref().get(name)
    }
}
