use crate::args::{Arguments, TimeThreshold};
use crate::bench::Bencher;
use crate::stats::Summary;
use std::any::{Any, TypeId};
use std::backtrace::Backtrace;
use std::cmp::{max, Ordering};
use std::fmt::{Debug, Display, Formatter};
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub enum TestFunction {
    Sync(
        Arc<
            dyn Fn(Arc<dyn DependencyView + Send + Sync>) -> Box<dyn TestReturnValue>
                + Send
                + Sync
                + 'static,
        >,
    ),
    SyncBench(
        Arc<dyn Fn(&mut Bencher, Arc<dyn DependencyView + Send + Sync>) + Send + Sync + 'static>,
    ),
    #[cfg(feature = "tokio")]
    Async(
        Arc<
            dyn (Fn(
                    Arc<dyn DependencyView + Send + Sync>,
                ) -> Pin<Box<dyn Future<Output = Box<dyn TestReturnValue>>>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
    #[cfg(feature = "tokio")]
    AsyncBench(
        Arc<
            dyn for<'a> Fn(
                    &'a mut crate::bench::AsyncBencher,
                    Arc<dyn DependencyView + Send + Sync>,
                ) -> Pin<Box<dyn Future<Output = ()> + 'a>>
                + Send
                + Sync
                + 'static,
        >,
    ),
}

impl TestFunction {
    #[cfg(not(feature = "tokio"))]
    pub fn is_bench(&self) -> bool {
        matches!(self, TestFunction::SyncBench(_))
    }

    #[cfg(feature = "tokio")]
    pub fn is_bench(&self) -> bool {
        matches!(
            self,
            TestFunction::SyncBench(_) | TestFunction::AsyncBench(_)
        )
    }
}

pub trait TestReturnValue {
    fn into_result(self: Box<Self>) -> Result<(), FailureCause>;
}

impl TestReturnValue for () {
    fn into_result(self: Box<Self>) -> Result<(), FailureCause> {
        Ok(())
    }
}

impl<T, E: Display + Debug + Send + Sync + 'static> TestReturnValue for Result<T, E> {
    fn into_result(self: Box<Self>) -> Result<(), FailureCause> {
        match *self {
            Ok(_) => Ok(()),
            Err(e) => Err(FailureCause::from_error(e)),
        }
    }
}

#[derive(Clone)]
pub enum FailureCause {
    /// Test returned Err(e) where E: Display + Debug — stores both representations
    /// and the original error value for later downcasting
    ReturnedError {
        display: String,
        debug: String,
        prefer_debug: bool,
        error: Arc<dyn Any + Send + Sync>,
    },
    /// Test returned Err(String) — stored as raw string without formatting
    ReturnedMessage(String),
    /// Test panicked
    Panic(PanicCause),
    /// Framework error (join failure, timeout, IPC deserialization, etc.)
    HarnessError(String),
}

#[derive(Debug, Clone)]
pub struct PanicCause {
    pub message: Option<String>,
    pub location: Option<PanicLocation>,
    pub backtrace: Option<Arc<Backtrace>>,
}

#[derive(Debug, Clone)]
pub struct PanicLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

impl std::fmt::Debug for FailureCause {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FailureCause::ReturnedError { display, .. } => {
                f.debug_tuple("ReturnedError").field(display).finish()
            }
            FailureCause::ReturnedMessage(s) => f.debug_tuple("ReturnedMessage").field(s).finish(),
            FailureCause::Panic(p) => f.debug_tuple("Panic").field(p).finish(),
            FailureCause::HarnessError(s) => f.debug_tuple("HarnessError").field(s).finish(),
        }
    }
}

impl FailureCause {
    pub fn from_error<E: Display + Debug + Send + Sync + 'static>(e: E) -> Self {
        if TypeId::of::<E>() == TypeId::of::<String>() {
            let any: Box<dyn Any + Send + Sync> = Box::new(e);
            return FailureCause::ReturnedMessage(*any.downcast::<String>().unwrap());
        }

        let mut _prefer_debug = false;
        #[cfg(feature = "anyhow")]
        {
            _prefer_debug = TypeId::of::<E>() == TypeId::of::<anyhow::Error>();
        }

        FailureCause::ReturnedError {
            display: format!("{e:#}"),
            debug: format!("{e:?}"),
            prefer_debug: _prefer_debug,
            error: Arc::new(e),
        }
    }

    pub fn render(&self) -> String {
        match self {
            FailureCause::ReturnedError {
                display,
                debug,
                prefer_debug,
                ..
            } => {
                if *prefer_debug {
                    debug.clone()
                } else {
                    display.clone()
                }
            }
            FailureCause::ReturnedMessage(s) => s.clone(),
            FailureCause::Panic(p) => p.render(),
            FailureCause::HarnessError(s) => s.clone(),
        }
    }

    /// Get the message string for ShouldPanic matching (without backtrace)
    pub fn panic_message(&self) -> Option<&str> {
        match self {
            FailureCause::Panic(p) => p.message.as_deref(),
            _ => None,
        }
    }
}

impl PanicCause {
    pub fn render(&self) -> String {
        let mut out = self.message.clone().unwrap_or_default();
        if let Some(loc) = &self.location {
            out.push_str(&format!("\n  at {}:{}:{}", loc.file, loc.line, loc.column));
        }
        if let Some(bt) = &self.backtrace {
            let bt_str = format!("{bt}");
            if !bt_str.is_empty() && bt_str != "disabled backtrace" {
                out.push_str(&format!("\n\nStack backtrace:\n{bt}"));
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShouldPanic {
    No,
    Yes,
    WithMessage(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestType {
    UnitTest,
    IntegrationTest,
}

impl TestType {
    pub fn from_path(path: &str) -> Self {
        if path.contains("/src/") {
            TestType::UnitTest
        } else {
            TestType::IntegrationTest
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlakinessControl {
    None,
    ProveNonFlaky(usize),
    RetryKnownFlaky(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachedPanicPolicy {
    FailTest,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureControl {
    Default,
    AlwaysCapture,
    NeverCapture,
}

impl CaptureControl {
    pub fn requires_capturing(&self, default: bool) -> bool {
        match self {
            CaptureControl::Default => default,
            CaptureControl::AlwaysCapture => true,
            CaptureControl::NeverCapture => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportTimeControl {
    Default,
    Enabled,
    Disabled,
}

#[derive(Clone)]
pub struct TestProperties {
    pub should_panic: ShouldPanic,
    pub test_type: TestType,
    pub timeout: Option<Duration>,
    pub flakiness_control: FlakinessControl,
    pub capture_control: CaptureControl,
    pub report_time_control: ReportTimeControl,
    pub ensure_time_control: ReportTimeControl,
    pub tags: Vec<String>,
    pub is_ignored: bool,
    pub detached_panic_policy: DetachedPanicPolicy,
}

impl TestProperties {
    pub fn unit_test() -> Self {
        TestProperties {
            test_type: TestType::UnitTest,
            ..Default::default()
        }
    }

    pub fn integration_test() -> Self {
        TestProperties {
            test_type: TestType::IntegrationTest,
            ..Default::default()
        }
    }
}

impl Default for TestProperties {
    fn default() -> Self {
        Self {
            should_panic: ShouldPanic::No,
            test_type: TestType::UnitTest,
            timeout: None,
            flakiness_control: FlakinessControl::None,
            capture_control: CaptureControl::Default,
            report_time_control: ReportTimeControl::Default,
            ensure_time_control: ReportTimeControl::Default,
            tags: Vec::new(),
            is_ignored: false,
            detached_panic_policy: DetachedPanicPolicy::FailTest,
        }
    }
}

#[derive(Clone)]
pub struct RegisteredTest {
    pub name: String,
    pub crate_name: String,
    pub module_path: String,
    pub run: TestFunction,
    pub props: TestProperties,
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
            dyn (Fn(Arc<dyn DependencyView + Send + Sync>) -> Arc<dyn Any + Send + Sync + 'static>)
                + Send
                + Sync
                + 'static,
        >,
    ),
    Async(
        Arc<
            dyn (Fn(
                    Arc<dyn DependencyView + Send + Sync>,
                ) -> Pin<Box<dyn Future<Output = Arc<dyn Any + Send + Sync>>>>)
                + Send
                + Sync
                + 'static,
        >,
    ),
}

#[derive(Clone)]
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
    Tag {
        name: String,
        crate_name: String,
        module_path: String,
        tag: String,
    },
}

impl RegisteredTestSuiteProperty {
    pub fn crate_name(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { crate_name, .. } => crate_name,
            RegisteredTestSuiteProperty::Tag { crate_name, .. } => crate_name,
        }
    }

    pub fn module_path(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { module_path, .. } => module_path,
            RegisteredTestSuiteProperty::Tag { module_path, .. } => module_path,
        }
    }

    pub fn name(&self) -> &String {
        match self {
            RegisteredTestSuiteProperty::Sequential { name, .. } => name,
            RegisteredTestSuiteProperty::Tag { name, .. } => name,
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

    pub fn add_sync_test<R: TestReturnValue + 'static>(
        &mut self,
        name: impl AsRef<str>,
        props: TestProperties,
        run: impl Fn(Arc<dyn DependencyView + Send + Sync>) -> R + Send + Sync + Clone + 'static,
    ) {
        self.tests.push(GeneratedTest {
            name: name.as_ref().to_string(),
            run: TestFunction::Sync(Arc::new(move |deps| {
                Box::new(run(deps)) as Box<dyn TestReturnValue>
            })),
            props,
        });
    }

    #[cfg(feature = "tokio")]
    pub fn add_async_test<R: TestReturnValue + 'static>(
        &mut self,
        name: impl AsRef<str>,
        props: TestProperties,
        run: impl (Fn(Arc<dyn DependencyView + Send + Sync>) -> Pin<Box<dyn Future<Output = R> + Send>>)
            + Send
            + Sync
            + Clone
            + 'static,
    ) {
        self.tests.push(GeneratedTest {
            name: name.as_ref().to_string(),
            run: TestFunction::Async(Arc::new(move |deps| {
                let run = run.clone();
                Box::pin(async move {
                    let r = run(deps).await;
                    Box::new(r) as Box<dyn TestReturnValue>
                })
            })),
            props,
        });
    }
}

#[derive(Clone)]
pub struct GeneratedTest {
    pub name: String,
    pub run: TestFunction,
    pub props: TestProperties,
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
    if let Some(tag_list) = filter.strip_prefix(":tag:") {
        if tag_list.is_empty() {
            // Filtering for tags with NO TAGS
            test.props.tags.is_empty()
        } else {
            let or_tags = tag_list.split('|').collect::<Vec<&str>>();
            let mut result = false;
            for or_tag in or_tags {
                let and_tags = or_tag.split('&').collect::<Vec<&str>>();
                let mut and_result = true;
                for and_tag in and_tags {
                    if !test.props.tags.contains(&and_tag.to_string()) {
                        and_result = false;
                        break;
                    }
                }
                if and_result {
                    result = true;
                    break;
                }
            }
            result
        }
    } else if exact {
        test.filterable_name() == filter
    } else {
        test.filterable_name().contains(filter)
    }
}

pub(crate) fn apply_suite_tags(
    tests: &[RegisteredTest],
    props: &[RegisteredTestSuiteProperty],
) -> Vec<RegisteredTest> {
    let tag_props = props
        .iter()
        .filter_map(|prop| match prop {
            RegisteredTestSuiteProperty::Tag { tag, .. } => {
                let prefix = prop.crate_and_module();
                Some((prefix, tag.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut result = Vec::new();
    for test in tests {
        let mut test = test.clone();
        for (prefix, tag) in &tag_props {
            if &test.crate_and_module() == prefix {
                test.props.tags.push(tag.clone());
            }
        }
        result.push(test);
    }
    result
}

pub(crate) fn filter_registered_tests(
    args: &Arguments,
    registered_tests: &[RegisteredTest],
) -> Vec<RegisteredTest> {
    registered_tests
        .iter()
        .filter(|registered_test| {
            args.skip
                .iter()
                .all(|skip| &registered_test.filterable_name() != skip)
        })
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
        .filter(|registered_test| {
            !args.exclude_should_panic || registered_test.props.should_panic == ShouldPanic::No
        })
        .cloned()
        .collect::<Vec<_>>()
}

fn add_generated_tests(
    target: &mut Vec<RegisteredTest>,
    generator: &RegisteredTestGenerator,
    generated: Vec<GeneratedTest>,
) {
    target.extend(generated.into_iter().map(|mut test| {
        test.props.is_ignored |= generator.is_ignored;
        RegisteredTest {
            name: format!("{}::{}", generator.name, test.name),
            crate_name: generator.crate_name.clone(),
            module_path: generator.module_path.clone(),
            run: test.run,
            props: test.props,
        }
    }));
}

#[cfg(feature = "tokio")]
pub(crate) async fn generate_tests(generators: &[RegisteredTestGenerator]) -> Vec<RegisteredTest> {
    let mut result = Vec::new();
    for generator in generators {
        match &generator.run {
            TestGeneratorFunction::Sync(generator_fn) => {
                let tests = generator_fn();
                add_generated_tests(&mut result, generator, tests);
            }
            TestGeneratorFunction::Async(generator_fn) => {
                let tests = generator_fn().await;
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
                let tests = generator_fn();
                add_generated_tests(&mut result, generator, tests);
            }
            TestGeneratorFunction::Async(_) => {
                panic!("Async test generators are not supported in sync mode")
            }
        }
    }
    result
}

pub(crate) fn get_ensure_time(args: &Arguments, test: &RegisteredTest) -> Option<TimeThreshold> {
    let should_ensure_time = match test.props.ensure_time_control {
        ReportTimeControl::Default => args.ensure_time,
        ReportTimeControl::Enabled => true,
        ReportTimeControl::Disabled => false,
    };
    if should_ensure_time {
        match test.props.test_type {
            TestType::UnitTest => Some(args.unit_test_threshold()),
            TestType::IntegrationTest => Some(args.integration_test_threshold()),
        }
    } else {
        None
    }
}

#[derive(Clone)]
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
        cause: FailureCause,
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

    pub fn failed(exec_time: Duration, cause: FailureCause) -> Self {
        TestResult::Failed {
            cause,
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

    pub(crate) fn captured_output(&self) -> &Vec<CapturedOutput> {
        match self {
            TestResult::Passed { captured, .. } => captured,
            TestResult::Failed { captured, .. } => captured,
            TestResult::Ignored { captured, .. } => captured,
            TestResult::Benchmarked { captured, .. } => captured,
        }
    }

    pub(crate) fn stats(&self) -> Option<&Summary> {
        match self {
            TestResult::Benchmarked { ns_iter_summ, .. } => Some(ns_iter_summ),
            _ => None,
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
        result: Result<Result<A, FailureCause>, Box<dyn Any + Send>>,
    ) -> Self {
        match result {
            Ok(Ok(_)) => {
                if should_panic == &ShouldPanic::No {
                    TestResult::passed(elapsed)
                } else {
                    TestResult::failed(
                        elapsed,
                        FailureCause::HarnessError("Test did not panic as expected".to_string()),
                    )
                }
            }
            Ok(Err(cause)) => TestResult::failed(elapsed, cause),
            Err(panic) => TestResult::from_panic(should_panic, elapsed, panic),
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
        let captured = crate::panic_hook::take_current_panic_capture();

        let panic_cause = if let Some(cause) = captured {
            cause
        } else {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or(panic.downcast_ref::<&str>().map(|s| s.to_string()));
            PanicCause {
                message,
                location: None,
                backtrace: None,
            }
        };

        match should_panic {
            ShouldPanic::WithMessage(expected) => match &panic_cause.message {
                Some(message) if message.contains(expected) => TestResult::passed(elapsed),
                _ => TestResult::failed(
                    elapsed,
                    FailureCause::Panic(PanicCause {
                        message: Some(format!(
                            "Test panicked with unexpected message: {}",
                            panic_cause.message.as_deref().unwrap_or_default()
                        )),
                        location: None,
                        backtrace: None,
                    }),
                ),
            },
            ShouldPanic::Yes => TestResult::passed(elapsed),
            ShouldPanic::No => TestResult::failed(elapsed, FailureCause::Panic(panic_cause)),
        }
    }

    pub(crate) fn failure_message(&self) -> Option<String> {
        self.failure_cause().map(|c| c.render())
    }

    pub fn failure_cause(&self) -> Option<&FailureCause> {
        match self {
            TestResult::Failed { cause, .. } => Some(cause),
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

    pub fn exit_code(results: &[(RegisteredTest, TestResult)]) -> ExitCode {
        if results.iter().any(|(_, result)| result.is_failed()) {
            ExitCode::from(101)
        } else {
            ExitCode::SUCCESS
        }
    }
}

pub trait DependencyView: Debug {
    fn get(&self, name: &str) -> Option<Arc<dyn Any + Send + Sync>>;
}

impl DependencyView for Arc<dyn DependencyView + Send + Sync> {
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

    pub fn line(&self) -> &str {
        match self {
            CapturedOutput::Stdout { line, .. } => line,
            CapturedOutput::Stderr { line, .. } => line,
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

#[cfg(test)]
mod error_reporting_tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::time::Duration;

    fn simulate_runner(
        test_fn: impl FnOnce() -> Box<dyn TestReturnValue> + std::panic::UnwindSafe,
    ) -> TestResult {
        crate::panic_hook::install_panic_hook();
        let test_id = crate::panic_hook::next_test_id();
        crate::panic_hook::set_current_test_id(test_id);
        let result = catch_unwind(AssertUnwindSafe(move || {
            let ret = test_fn();
            ret.into_result()?;
            Ok(())
        }));
        let test_result =
            TestResult::from_result(&ShouldPanic::No, Duration::from_millis(1), result);
        crate::panic_hook::clear_current_test_id();
        test_result
    }

    #[test]
    fn panic_with_assert_eq() {
        let result = simulate_runner(|| {
            assert_eq!(1, 2);
            Box::new(())
        });
        assert!(result.is_failed());
        let msg = result.failure_message().unwrap();
        println!("=== panic assert_eq failure message ===\n{msg}\n===");
        assert!(
            msg.contains("assertion `left == right` failed"),
            "Expected assertion message, got: {msg}"
        );
        assert!(
            msg.contains("at "),
            "Expected location info in message, got: {msg}"
        );
    }

    #[test]
    fn string_error() {
        let result = simulate_runner(|| {
            let r: Result<(), String> = Err("something went wrong".to_string());
            Box::new(r)
        });
        assert!(result.is_failed());
        let msg = result.failure_message().unwrap();
        println!("=== string error failure message ===\n{msg}\n===");
        assert_eq!(msg, "something went wrong");
    }

    #[test]
    fn anyhow_error() {
        let result = simulate_runner(|| {
            let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
            let err = anyhow::anyhow!(inner).context("operation failed");
            let r: Result<(), anyhow::Error> = Err(err);
            Box::new(r)
        });
        assert!(result.is_failed());
        let msg = result.failure_message().unwrap();
        println!("=== anyhow error failure message ===\n{msg}\n===");
        assert!(
            msg.contains("operation failed"),
            "Expected 'operation failed', got: {msg}"
        );
        assert!(
            msg.contains("file not found"),
            "Expected 'file not found', got: {msg}"
        );
    }

    #[test]
    fn std_io_error() {
        let result = simulate_runner(|| {
            let r: Result<(), std::io::Error> = Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "file not found",
            ));
            Box::new(r)
        });
        assert!(result.is_failed());
        let msg = result.failure_message().unwrap();
        println!("=== std io error failure message ===\n{msg}\n===");
        // Should use Display (not Debug), so no "Custom { kind: NotFound, ... }"
        assert_eq!(msg, "file not found");
    }

    #[test]
    fn panic_with_location_info() {
        let result = simulate_runner(|| {
            panic!("test panic with location");
            #[allow(unreachable_code)]
            Box::new(())
        });
        assert!(result.is_failed());
        let cause = result.failure_cause().unwrap();
        match cause {
            FailureCause::Panic(p) => {
                assert!(p.location.is_some(), "Expected location info");
                let loc = p.location.as_ref().unwrap();
                assert!(
                    loc.file.contains("internal.rs"),
                    "Expected file to contain internal.rs, got: {}",
                    loc.file
                );
                assert!(loc.line > 0, "Expected non-zero line number");
            }
            other => panic!("Expected Panic cause, got: {other:?}"),
        }
    }

    #[test]
    fn panic_render_includes_location() {
        let result = simulate_runner(|| {
            panic!("location test");
            #[allow(unreachable_code)]
            Box::new(())
        });
        let msg = result.failure_message().unwrap();
        assert!(
            msg.contains("location test"),
            "Expected panic message, got: {msg}"
        );
        assert!(
            msg.contains("\n  at "),
            "Expected location line in render, got: {msg}"
        );
    }

    #[test]
    fn should_panic_with_message_matching() {
        crate::panic_hook::install_panic_hook();
        let test_id = crate::panic_hook::next_test_id();
        crate::panic_hook::set_current_test_id(test_id);
        let result = catch_unwind(AssertUnwindSafe(|| {
            panic!("expected panic message");
        }));
        let test_result = TestResult::from_result(
            &ShouldPanic::WithMessage("expected panic".to_string()),
            Duration::from_millis(1),
            result.map(|_| Ok(())),
        );
        crate::panic_hook::clear_current_test_id();
        assert!(
            test_result.is_passed(),
            "Expected test to pass with matching panic message"
        );
    }

    #[test]
    fn should_panic_with_wrong_message() {
        crate::panic_hook::install_panic_hook();
        let test_id = crate::panic_hook::next_test_id();
        crate::panic_hook::set_current_test_id(test_id);
        let result = catch_unwind(AssertUnwindSafe(|| {
            panic!("actual panic message");
        }));
        let test_result = TestResult::from_result(
            &ShouldPanic::WithMessage("completely different".to_string()),
            Duration::from_millis(1),
            result.map(|_| Ok(())),
        );
        crate::panic_hook::clear_current_test_id();
        assert!(
            test_result.is_failed(),
            "Expected test to fail with wrong panic message"
        );
        let msg = test_result.failure_message().unwrap();
        assert!(
            msg.contains("unexpected message"),
            "Expected 'unexpected message' in: {msg}"
        );
    }

    #[test]
    fn pretty_assertions_diff() {
        let result = simulate_runner(|| {
            pretty_assertions::assert_eq!("hello world\nfoo\nbar\n", "hello world\nbaz\nbar\n");
            Box::new(())
        });
        assert!(result.is_failed());
        let cause = result.failure_cause().unwrap();

        // Should be a Panic variant (assert_eq! panics)
        let panic_cause = match cause {
            FailureCause::Panic(p) => p,
            other => panic!("Expected Panic cause, got: {other:?}"),
        };

        // The panic message should contain the colorful diff from pretty_assertions
        let message = panic_cause.message.as_deref().unwrap();
        println!("=== pretty_assertions failure message ===\n{message}\n===");
        assert!(
            message.contains("foo") && message.contains("baz"),
            "Expected diff with 'foo' and 'baz', got: {message}"
        );

        // Location should be captured
        assert!(panic_cause.location.is_some(), "Expected location info");

        // The rendered output should NOT contain backtrace noise when RUST_BACKTRACE is unset
        let rendered = cause.render();
        println!("=== pretty_assertions rendered ===\n{rendered}\n===");
        assert!(
            !rendered.contains("stack backtrace") && !rendered.contains("Stack backtrace"),
            "Expected no backtrace noise in rendered output, got: {rendered}"
        );
        // Should contain location
        assert!(
            rendered.contains("\n  at "),
            "Expected location in rendered output, got: {rendered}"
        );
    }

    #[test]
    fn failure_cause_variants() {
        // ReturnedMessage
        let cause = FailureCause::ReturnedMessage("simple message".to_string());
        assert_eq!(cause.render(), "simple message");
        assert!(cause.panic_message().is_none());

        // ReturnedError (prefer display)
        let cause = FailureCause::ReturnedError {
            display: "display text".to_string(),
            debug: "debug text".to_string(),
            prefer_debug: false,
            error: Arc::new("display text".to_string()),
        };
        assert_eq!(cause.render(), "display text");

        // ReturnedError (prefer debug, e.g. anyhow)
        let cause = FailureCause::ReturnedError {
            display: "display text".to_string(),
            debug: "debug text".to_string(),
            prefer_debug: true,
            error: Arc::new("debug text".to_string()),
        };
        assert_eq!(cause.render(), "debug text");

        // HarnessError
        let cause = FailureCause::HarnessError("harness error".to_string());
        assert_eq!(cause.render(), "harness error");

        // Panic with message
        let cause = FailureCause::Panic(PanicCause {
            message: Some("panic msg".to_string()),
            location: None,
            backtrace: None,
        });
        assert_eq!(cause.render(), "panic msg");
        assert_eq!(cause.panic_message(), Some("panic msg"));
    }
}
