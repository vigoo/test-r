use crate::args::Arguments;
use crate::bench::{AsyncBencher, Bencher};
use crate::stats::Summary;
use std::any::Any;
use std::cmp::{max, Ordering};
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub enum TestFunction {
    Sync(Arc<dyn Fn(Box<dyn DependencyView + Send + Sync>) + Send + Sync + 'static>),
    SyncBench(
        Arc<dyn Fn(&mut Bencher, Box<dyn DependencyView + Send + Sync>) + Send + Sync + 'static>,
    ),
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
    AsyncBench(
        Arc<
            dyn Fn(&mut AsyncBencher, Box<dyn DependencyView + Send + Sync>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

impl TestFunction {
    pub fn is_bench(&self) -> bool {
        matches!(
            self,
            TestFunction::SyncBench(_) | TestFunction::AsyncBench(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShouldPanic {
    No,
    Yes,
    WithMessage(String),
}

#[derive(Clone)]
pub struct RegisteredTest {
    pub name: String,
    pub crate_name: String,
    pub module_path: String,
    pub is_ignored: bool,
    pub should_panic: ShouldPanic,
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
#[allow(clippy::type_complexity)]
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
#[allow(clippy::type_complexity)]
pub enum TestGeneratorFunction {
    Sync(Arc<dyn Fn() -> Vec<GeneratedTest> + Send + Sync + 'static>),
    Async(
        Arc<
            dyn (Fn() -> Pin<Box<dyn Future<Output = Vec<GeneratedTest>> + Send>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

pub struct DynamicTestRegistration {
    tests: Vec<GeneratedTest>,
}

impl Default for DynamicTestRegistration {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicTestRegistration {
    pub fn new() -> Self {
        Self { tests: Vec::new() }
    }

    pub fn to_vec(self) -> Vec<GeneratedTest> {
        self.tests
    }

    pub fn add_sync_test(
        &mut self,
        name: impl AsRef<str>,
        run: impl Fn(Box<dyn DependencyView + Send + Sync>) + Send + Sync + 'static,
    ) {
        self.tests.push(GeneratedTest {
            name: name.as_ref().to_string(),
            run: TestFunction::Sync(Arc::new(run)),
        });
    }

    #[cfg(feature = "tokio")]
    pub fn add_async_test(
        &mut self,
        name: impl AsRef<str>,
        run: impl (Fn(Box<dyn DependencyView + Send + Sync>) -> Pin<Box<dyn Future<Output = ()> + Send>>)
            + Send
            + Sync
            + 'static,
    ) {
        self.tests.push(GeneratedTest {
            name: name.as_ref().to_string(),
            run: TestFunction::Async(Arc::new(run)),
        });
    }
}

#[derive(Clone)]
pub struct GeneratedTest {
    pub name: String,
    pub run: TestFunction,
}

#[derive(Clone)]
pub struct RegisteredTestGenerator {
    pub name: String,
    pub crate_name: String,
    pub module_path: String,
    pub run: TestGeneratorFunction,
    pub is_ignored: bool,
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

pub static REGISTERED_TEST_GENERATORS: Mutex<Vec<RegisteredTestGenerator>> = Mutex::new(Vec::new());

pub(crate) fn filter_test(test: &RegisteredTest, filter: &str, exact: bool) -> bool {
    if exact {
        test.filterable_name() == filter
    } else {
        test.filterable_name().contains(filter)
    }
}

pub(crate) fn filter_registered_tests<'a>(
    args: &Arguments,
    registered_tests: &'a [&'a RegisteredTest],
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
        .filter(|registered_tests| {
            (args.bench && registered_tests.run.is_bench())
                || (args.test && !registered_tests.run.is_bench())
                || (!args.bench && !args.test)
        })
        .copied()
        .collect::<Vec<_>>()
}

fn add_generated_tests(
    target: &mut Vec<RegisteredTest>,
    generator: &RegisteredTestGenerator,
    generated: Vec<GeneratedTest>,
) {
    target.extend(generated.into_iter().map(|test| RegisteredTest {
        name: format!("{}::{}", generator.name, test.name),
        crate_name: generator.crate_name.clone(),
        module_path: generator.module_path.clone(),
        is_ignored: generator.is_ignored,
        should_panic: ShouldPanic::No,
        run: test.run,
    }));
}

#[cfg(feature = "tokio")]
pub(crate) async fn generate_tests(generators: &[RegisteredTestGenerator]) -> Vec<RegisteredTest> {
    let mut result = Vec::new();
    for generator in generators {
        match &generator.run {
            TestGeneratorFunction::Sync(generator_fn) => {
                let tests = (generator_fn)();
                add_generated_tests(&mut result, generator, tests);
            }
            TestGeneratorFunction::Async(generator_fn) => {
                let tests = (generator_fn)().await;
                add_generated_tests(&mut result, generator, tests);
            }
        }
    }
    result
}

pub(crate) fn generate_tests_sync(generators: &[RegisteredTestGenerator]) -> Vec<RegisteredTest> {
    let mut result = Vec::new();
    for generator in generators {
        match &generator.run {
            TestGeneratorFunction::Sync(generator_fn) => {
                let tests = (generator_fn)();
                add_generated_tests(&mut result, generator, tests);
            }
            TestGeneratorFunction::Async(_) => {
                panic!("Async test generators are not supported in sync mode")
            }
        }
    }
    result
}

pub enum TestResult {
    Passed {
        captured: Vec<CapturedOutput>,
        exec_time: Duration,
    },
    Benchmarked {
        captured: Vec<CapturedOutput>,
        exec_time: Duration,
        ns_iter_summ: Summary,
        mb_s: usize,
    },
    Failed {
        panic: Box<dyn std::any::Any + Send>,
        captured: Vec<CapturedOutput>,
        exec_time: Duration,
    },
    Ignored {
        captured: Vec<CapturedOutput>,
    },
}

impl TestResult {
    pub fn passed(exec_time: Duration) -> Self {
        TestResult::Passed {
            captured: Vec::new(),
            exec_time,
        }
    }

    pub fn benchmarked(exec_time: Duration, ns_iter_summ: Summary, mb_s: usize) -> Self {
        TestResult::Benchmarked {
            captured: Vec::new(),
            exec_time,
            ns_iter_summ,
            mb_s,
        }
    }

    pub fn failed(exec_time: Duration, panic: Box<dyn std::any::Any + Send>) -> Self {
        TestResult::Failed {
            panic,
            captured: Vec::new(),
            exec_time,
        }
    }

    pub fn ignored() -> Self {
        TestResult::Ignored {
            captured: Vec::new(),
        }
    }

    pub(crate) fn is_passed(&self) -> bool {
        matches!(self, TestResult::Passed { .. })
    }

    pub(crate) fn is_benchmarked(&self) -> bool {
        matches!(self, TestResult::Benchmarked { .. })
    }

    pub(crate) fn is_failed(&self) -> bool {
        matches!(self, TestResult::Failed { .. })
    }

    pub(crate) fn is_ignored(&self) -> bool {
        matches!(self, TestResult::Ignored { .. })
    }

    pub(crate) fn failure_message(&self) -> Option<&str> {
        match self {
            TestResult::Failed { panic, .. } => panic
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or(panic.downcast_ref::<&str>().copied()),
            _ => None,
        }
    }

    pub(crate) fn captured_output(&self) -> &Vec<CapturedOutput> {
        match self {
            TestResult::Passed { captured, .. } => captured,
            TestResult::Failed { captured, .. } => captured,
            TestResult::Ignored { captured, .. } => captured,
            TestResult::Benchmarked { captured, .. } => captured,
        }
    }

    pub(crate) fn set_captured_output(&mut self, captured: Vec<CapturedOutput>) {
        match self {
            TestResult::Passed {
                captured: captured_ref,
                ..
            } => *captured_ref = captured,
            TestResult::Failed {
                captured: captured_ref,
                ..
            } => *captured_ref = captured,
            TestResult::Ignored {
                captured: captured_ref,
            } => *captured_ref = captured,
            TestResult::Benchmarked {
                captured: captured_ref,
                ..
            } => *captured_ref = captured,
        }
    }

    pub(crate) fn from_result<A>(
        should_panic: &ShouldPanic,
        elapsed: Duration,
        result: Result<A, Box<dyn Any + Send>>,
    ) -> Self {
        match result {
            Ok(_) => {
                if should_panic == &ShouldPanic::No {
                    TestResult::passed(elapsed)
                } else {
                    TestResult::failed(elapsed, Box::new("Test did not panic as expected"))
                }
            }
            Err(panic) => Self::from_panic(should_panic, elapsed, panic),
        }
    }

    pub(crate) fn from_summary(
        should_panic: &ShouldPanic,
        elapsed: Duration,
        result: Result<Summary, Box<dyn Any + Send>>,
        bytes: u64,
    ) -> Self {
        match result {
            Ok(summary) => {
                let ns_iter = max(summary.median as u64, 1);
                let mb_s = bytes * 1000 / ns_iter;
                TestResult::benchmarked(elapsed, summary, mb_s as usize)
            }
            Err(panic) => Self::from_panic(should_panic, elapsed, panic),
        }
    }

    fn from_panic(
        should_panic: &ShouldPanic,
        elapsed: Duration,
        panic: Box<dyn Any + Send>,
    ) -> Self {
        match should_panic {
            ShouldPanic::WithMessage(expected) => {
                let failure = TestResult::failed(elapsed, panic);
                let message = failure.failure_message();

                match message {
                    Some(message) if message.contains(expected) => TestResult::passed(elapsed),
                    _ => TestResult::failed(
                        elapsed,
                        Box::new(format!(
                            "Test panicked with unexpected message: {}",
                            message.unwrap_or_default()
                        )),
                    ),
                }
            }
            ShouldPanic::Yes => TestResult::passed(elapsed),
            ShouldPanic::No => TestResult::failed(elapsed, panic),
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
        registered_tests: &[&RegisteredTest],
        results: &[(RegisteredTest, TestResult)],
        exec_time: Duration,
    ) -> Self {
        let passed = results
            .iter()
            .filter(|(_, result)| result.is_passed())
            .count();
        let measured = results
            .iter()
            .filter(|(_, result)| result.is_benchmarked())
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
            measured,
            filtered_out,
            exec_time,
        }
    }
}

pub trait DependencyView: Debug {
    fn get(&self, name: &str) -> Option<Arc<dyn Any + Send + Sync>>;
}

impl DependencyView for Box<dyn DependencyView + Send + Sync> {
    fn get(&self, name: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.as_ref().get(name)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CapturedOutput {
    Stdout { timestamp: SystemTime, line: String },
    Stderr { timestamp: SystemTime, line: String },
}

impl CapturedOutput {
    pub fn stdout(line: String) -> Self {
        CapturedOutput::Stdout {
            timestamp: SystemTime::now(),
            line,
        }
    }

    pub fn stderr(line: String) -> Self {
        CapturedOutput::Stderr {
            timestamp: SystemTime::now(),
            line,
        }
    }

    pub fn timestamp(&self) -> SystemTime {
        match self {
            CapturedOutput::Stdout { timestamp, .. } => *timestamp,
            CapturedOutput::Stderr { timestamp, .. } => *timestamp,
        }
    }
}

impl PartialOrd for CapturedOutput {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CapturedOutput {
    fn cmp(&self, other: &Self) -> Ordering {
        self.timestamp().cmp(&other.timestamp())
    }
}
