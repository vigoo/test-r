pub use test_r_macro::add_test;
pub use test_r_macro::always_capture;
pub use test_r_macro::always_ensure_time;
pub use test_r_macro::always_report_time;
pub use test_r_macro::bench;
pub use test_r_macro::define_matrix_dimension;
pub use test_r_macro::flaky;
pub use test_r_macro::hosted_rpc;
pub use test_r_macro::ignore_detached_panics;
pub use test_r_macro::inherit_test_dep;
pub use test_r_macro::matrix_suite;
pub use test_r_macro::never_capture;
pub use test_r_macro::never_ensure_time;
pub use test_r_macro::never_report_time;
pub use test_r_macro::non_flaky;
pub use test_r_macro::sequential;
pub use test_r_macro::sequential_suite;
pub use test_r_macro::tag;
pub use test_r_macro::tag_suite;
pub use test_r_macro::test;
pub use test_r_macro::test_dep;
pub use test_r_macro::test_gen;
pub use test_r_macro::timeout;
pub use test_r_macro::timeout_suite;
pub use test_r_macro::uses_test_r as enable;

#[cfg(feature = "tokio")]
pub use test_r_core::bench::AsyncBencher;
pub use test_r_core::bench::Bencher;
#[cfg(feature = "tokio")]
pub use test_r_core::spawn::spawn;
pub use test_r_core::spawn::spawn_thread;

pub use test_r_core::internal::{
    AsyncHostedDep, AsyncHostedRpcDep, CloneableDep, HostedDep, HostedRpcDep,
};
pub use test_r_core::worker_index;

pub mod core {
    use std::time::Duration;
    pub use test_r_core::internal::{
        AsyncHostedDep, AsyncHostedRpcDep, AsyncHostedRpcDispatcher, CaptureControl,
        CloneableCodec, CloneableDep, DepScope, DependencyConstructor, DependencyView,
        DetachedPanicPolicy, DynamicTestRegistration, FailureCause, FlakinessControl,
        GeneratedTest, HostedBothShared, HostedDep, HostedRpcChannel, HostedRpcDep,
        HostedRpcDispatcher, HostedRpcError, HostedRpcOwnerCell, HostedRpcTransport,
        InProcessHostedRpcTransport, ReportTimeControl, RpcFactory, ShouldPanic, TestFunction,
        TestGeneratorFunction, TestProperties, TestReturnValue, TestType, WorkerReconstructor,
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
        tags: Vec<String>,
        report_time_control: ReportTimeControl,
        ensure_time_control: ReportTimeControl,
        detached_panic_policy: DetachedPanicPolicy,
        run: TestFunction,
        dependencies: Option<Vec<String>>,
    ) {
        let (crate_name, module_path) = split_module_path(module_path);

        internal::REGISTERED_TESTS
            .lock()
            .unwrap()
            .push(internal::RegisteredTest {
                name: name.to_string(),
                crate_name,
                module_path,
                run,
                props: internal::TestProperties {
                    should_panic,
                    test_type,
                    timeout,
                    flakiness_control,
                    capture_control,
                    report_time_control,
                    ensure_time_control,
                    tags,
                    is_ignored,
                    detached_panic_policy,
                },
                dependencies,
            });
    }

    pub fn register_dependency_constructor(
        name: &str,
        module_path: &str,
        cons: DependencyConstructor,
        dependencies: Vec<String>,
    ) {
        register_dependency_constructor_with_scope(
            name,
            module_path,
            cons,
            dependencies,
            DepScope::Shared,
            None,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_dependency_constructor_with_scope(
        name: &str,
        module_path: &str,
        cons: DependencyConstructor,
        dependencies: Vec<String>,
        scope: DepScope,
        worker_fn: Option<WorkerReconstructor>,
        cloneable_codec: Option<CloneableCodec>,
        hosted_codec: Option<CloneableCodec>,
        rpc_factory: Option<RpcFactory>,
    ) {
        register_dependency_constructor_with_scope_and_companions(
            name,
            module_path,
            cons,
            dependencies,
            scope,
            worker_fn,
            cloneable_codec,
            hosted_codec,
            rpc_factory,
            Vec::new(),
        )
    }

    /// Registers a dependency constructor that must be retained
    /// together with the listed `companions` during pruning. See
    /// [`internal::RegisteredDependency::companions`] for the planner
    /// semantics. All other parameters behave exactly as
    /// [`register_dependency_constructor_with_scope`].
    #[allow(clippy::too_many_arguments)]
    pub fn register_dependency_constructor_with_scope_and_companions(
        name: &str,
        module_path: &str,
        cons: DependencyConstructor,
        dependencies: Vec<String>,
        scope: DepScope,
        worker_fn: Option<WorkerReconstructor>,
        cloneable_codec: Option<CloneableCodec>,
        hosted_codec: Option<CloneableCodec>,
        rpc_factory: Option<RpcFactory>,
        companions: Vec<String>,
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
                scope,
                worker_fn,
                cloneable_codec,
                hosted_codec,
                rpc_factory,
                companions,
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

    pub fn register_suite_timeout(name: &str, module_path: &str, timeout: Duration) {
        let (crate_name, module_path) = split_module_path(module_path);

        internal::REGISTERED_TESTSUITE_PROPS.lock().unwrap().push(
            internal::RegisteredTestSuiteProperty::Timeout {
                name: name.to_string(),
                crate_name,
                module_path,
                timeout,
            },
        );
    }

    pub fn register_suite_tag(name: &str, module_path: &str, tag: String) {
        let (crate_name, module_path) = split_module_path(module_path);

        internal::REGISTERED_TESTSUITE_PROPS.lock().unwrap().push(
            internal::RegisteredTestSuiteProperty::Tag {
                name: name.to_string(),
                crate_name,
                module_path,
                tag,
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

/// **Hidden macro-support helper.** Runtime-flavor selector for code
/// emitted by `#[test_r::test_dep]` (specifically the
/// `worker = both(Trait)` lowering) that needs to pick a different
/// expression depending on whether the `test-r` crate was compiled
/// with its `tokio` feature.
///
/// The proc macro itself cannot read the user crate's cargo features,
/// so we route the runtime-flavor choice through this `macro_rules!`
/// definition in `test-r`. Because `#[cfg(feature = "tokio")]` on a
/// `macro_rules!` evaluates at the *defining* crate's compile time,
/// the variant of the macro that gets exported reflects whether
/// `test-r/tokio` was enabled — exactly the same toggle that decides
/// which `test-r-core` helper variants are linked.
///
/// The expected invocation shape is:
///
/// ```ignore
/// test_r::__test_r_select_runtime! {
///     sync { /* tokens used when the sync runtime is active */ }
///     tokio { /* tokens used when the tokio runtime is active */ }
/// }
/// ```
///
/// Each branch is a brace-delimited token group; the macro expands to
/// the contents of the matching branch with no extra braces.
#[cfg(feature = "tokio")]
#[doc(hidden)]
#[macro_export]
macro_rules! __test_r_select_runtime {
    ( sync { $($_sync:tt)* } tokio { $($tokio:tt)* } ) => {
        $($tokio)*
    };
}

/// Sync-runtime variant of [`__test_r_select_runtime`]; see that
/// macro's doc-comment.
#[cfg(not(feature = "tokio"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __test_r_select_runtime {
    ( sync { $($sync:tt)* } tokio { $($_tokio:tt)* } ) => {
        $($sync)*
    };
}
