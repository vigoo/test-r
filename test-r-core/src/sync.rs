use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::args;
use crate::internal;
use crate::internal::{filter_registered_tests, RegisteredTest, TestFunction};
use crate::output::test_runner_output;

pub fn test_runner() {
    let args = args::Arguments::from_args();
    let mut output = test_runner_output(&args);

    let registered_tests = internal::REGISTERED_TESTS.lock().unwrap();
    let filtered = filter_registered_tests(&args, &registered_tests);
    let count = filtered.len();
    let mut results = Vec::with_capacity(count);

    if args.list {
        output.test_list(&*registered_tests);
    } else {
        output.start_suite(count);

        for (idx, registered_test) in filtered.into_iter().enumerate() {
            output.start_running_test(registered_test, idx, count);
            let result = run_test(args.include_ignored, registered_test);
            output.finished_running_test(registered_test, idx, count, &result);

            results.push((registered_test, result));
        }

        output.finished_suite(&*registered_tests, &results);
    }
}

fn run_test(include_ignored: bool, test: &RegisteredTest) -> internal::TestResult {
    if test.is_ignored && !include_ignored {
        internal::TestResult::Ignored
    } else {
        let test_fn = &test.run;
        run_sync_test_function(test_fn)
    }
}

#[allow(unreachable_patterns)]
pub(crate) fn run_sync_test_function(test_fn: &TestFunction) -> internal::TestResult {
    let result = catch_unwind(AssertUnwindSafe(move || match test_fn {
        TestFunction::Sync(test_fn) => test_fn(),
        _ => {
            panic!("Async tests are not supported in sync mode, enable the 'tokio' feature")
        }
    }));
    match result {
        Ok(_) => internal::TestResult::Passed,
        Err(panic) => internal::TestResult::Failed { panic },
    }
}
