pub use test_r_macro::add_test;
pub use test_r_macro::always_capture;
pub use test_r_macro::bench;
pub use test_r_macro::flaky;
pub use test_r_macro::inherit_test_dep;
pub use test_r_macro::never_capture;
pub use test_r_macro::non_flaky;
pub use test_r_macro::sequential;
pub use test_r_macro::test;
pub use test_r_macro::test_dep;
pub use test_r_macro::test_gen;
pub use test_r_macro::timeout;
pub use test_r_macro::uses_test_r as enable;

#[cfg(feature = "tokio")]
pub use test_r_core::bench::AsyncBencher;
pub use test_r_core::bench::Bencher;

pub mod core {
    use std::time::Duration;
    pub use test_r_core::internal::{
        CaptureControl, DependencyConstructor, DependencyView, DynamicTestRegistration,
        FlakinessControl, GeneratedTest, ShouldPanic, TestFunction, TestGeneratorFunction,
        TestType,
    };
    pub use test_r_core::*;

    #[allow(clippy::too_many_arguments)]
    pub fn register_test(
        name: &str,
        module_path: &str,
        is_ignored: bool,
        should_panic: ShouldPanic,
        test_type: TestType,
        timeout: Option<Duration>,
        flakiness_control: FlakinessControl,
        capture_control: CaptureControl,
        run: TestFunction,
    ) {
        let (crate_name, module_path) = split_module_path(module_path);

        internal::REGISTERED_TESTS
            .lock()
            .unwrap()
            .push(internal::RegisteredTest {
                name: name.to_string(),
                crate_name,
                module_path,
                is_ignored,
                should_panic,
                run,
                test_type,
                timeout,
                flakiness_control,
                capture_control,
            });
    }

    pub fn register_dependency_constructor(
        name: &str,
        module_path: &str,
        cons: DependencyConstructor,
        dependencies: Vec<String>,
    ) {
        let (crate_name, module_path) = split_module_path(module_path);

        internal::REGISTERED_DEPENDENCY_CONSTRUCTORS
            .lock()
            .unwrap()
            .push(internal::RegisteredDependency {
                name: name.to_string(),
                crate_name,
                module_path,
                constructor: cons,
                dependencies,
            });
    }

    pub fn register_suite_sequential(name: &str, module_path: &str) {
        let (crate_name, module_path) = split_module_path(module_path);

        internal::REGISTERED_TESTSUITE_PROPS.lock().unwrap().push(
            internal::RegisteredTestSuiteProperty::Sequential {
                name: name.to_string(),
                crate_name,
                module_path,
            },
        );
    }

    pub fn register_test_generator(
        name: &str,
        module_path: &str,
        is_ignored: bool,
        run: TestGeneratorFunction,
    ) {
        let (crate_name, module_path) = split_module_path(module_path);

        internal::REGISTERED_TEST_GENERATORS.lock().unwrap().push(
            internal::RegisteredTestGenerator {
                name: name.to_string(),
                crate_name,
                module_path,
                run,
                is_ignored,
            },
        );
    }

    fn split_module_path(module_path: &str) -> (String, String) {
        let (crate_name, module_path) =
            if let Some((crate_name, module_path)) = module_path.split_once("::") {
                (crate_name.to_string(), module_path.to_string())
            } else {
                (module_path.to_string(), String::new())
            };
        (crate_name, module_path)
    }
}

pub use ::ctor;
