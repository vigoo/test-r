use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::args;
use crate::execution::TestSuiteExecution;
use crate::internal;
use crate::internal::{RegisteredTest, TestFunction};
use crate::output::test_runner_output;

pub fn test_runner() {
    let args = args::Arguments::from_args();
    let mut output = test_runner_output(&args);

    let registered_tests = internal::REGISTERED_TESTS.lock().unwrap();
    let registered_dependency_constructors =
        internal::REGISTERED_DEPENDENCY_CONSTRUCTORS.lock().unwrap();

    if args.list {
        output.test_list(&registered_tests);
    } else {
        let mut execution = TestSuiteExecution::construct(
            &args,
            registered_dependency_constructors.as_slice(),
            registered_tests.as_slice(),
        );
        // println!("Execution plan: {execution:?}");

        let count = execution.remaining();
        let mut results = Vec::with_capacity(count);

        output.start_suite(count);

        let mut idx = 0; // TODO: track this within execution
        while let Some((registered_test, deps)) = execution.pick_next_sync() {
            output.start_running_test(registered_test, idx, count);
            let result = run_test(args.include_ignored, deps, registered_test);
            output.finished_running_test(registered_test, idx, count, &result);

            results.push((registered_test, result));
            idx += 1;
        }

        output.finished_suite(&registered_tests, &results);
    }
}

fn run_test(
    include_ignored: bool,
    dependency_view: Box<dyn internal::DependencyView + Send + Sync>,
    test: &RegisteredTest,
) -> internal::TestResult {
    if test.is_ignored && !include_ignored {
        internal::TestResult::Ignored
    } else {
        let test_fn = &test.run;
        run_sync_test_function(test_fn, dependency_view)
    }
}

#[allow(unreachable_patterns)]
pub(crate) fn run_sync_test_function(
    test_fn: &TestFunction,
    dependency_view: Box<dyn internal::DependencyView + Send + Sync>,
) -> internal::TestResult {
    let result = catch_unwind(AssertUnwindSafe(move || match test_fn {
        TestFunction::Sync(test_fn) => test_fn(dependency_view),
        _ => {
            panic!("Async tests are not supported in sync mode, enable the 'tokio' feature")
        }
    }));
    match result {
        Ok(_) => internal::TestResult::Passed,
        Err(panic) => internal::TestResult::Failed { panic },
    }
}
